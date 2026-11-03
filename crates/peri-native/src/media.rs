//! Typed media resources and local dependency/deadline enforcement.
//! 媒体资源类型与本地依赖/截止时间处理，解码成功必须由调用方确认。

use crate::{Error, MAX_OBJECT_BYTES, Object, Session};
use peri_types::Agreement;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

const CATALOG_MAGIC: &[u8; 6] = b"PCAT01";
const MAX_CATALOG: usize = 16 * 1024;
const MAX_DEPENDENCIES: usize = 8;
const MAX_HISTORY: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
}

/// Decoder configuration for a negotiated track; init bytes remain codec-specific.
/// 已协商轨道的解码配置；初始化字节的含义由具体编码定义。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub id: u16,
    pub kind: TrackKind,
    pub codec: String,
    pub init: Vec<u8>,
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// Immutable track catalog for one media session.
/// 单次媒体会话的不可变轨道目录；配置变化需要重建会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub tracks: Vec<Track>,
}
impl Catalog {
    /// Validate track bounds and negotiated codec membership.
    /// 校验轨道边界与协商得到的编码集合。
    pub fn validate(&self, agreement: &Agreement) -> Result<(), Error> {
        if self.tracks.is_empty() || self.tracks.len() > 8 {
            return Err(Error::Protocol("track count"));
        }
        let mut ids = HashSet::new();
        for track in &self.tracks {
            if track.id == 0
                || !ids.insert(track.id)
                || track.init.len() > 4096
                || !agreement.codecs.contains(&track.codec)
            {
                return Err(Error::Protocol("track configuration"));
            }
            match track.kind {
                TrackKind::Video
                    if track.codec.starts_with("video/")
                        && track.width.is_some_and(|v| (1..=8192).contains(&v))
                        && track.height.is_some_and(|v| (1..=8192).contains(&v))
                        && track.sample_rate.is_none()
                        && track.channels.is_none() => {}
                TrackKind::Audio
                    if track.codec.starts_with("audio/")
                        && track
                            .sample_rate
                            .is_some_and(|v| (8000..=192000).contains(&v))
                        && track.channels.is_some_and(|v| (1..=8).contains(&v))
                        && track.width.is_none()
                        && track.height.is_none() => {}
                _ => return Err(Error::Protocol("track media properties")),
            }
        }
        if serde_json::to_vec(self)?.len() + CATALOG_MAGIC.len() > MAX_CATALOG {
            return Err(Error::Protocol("catalog length"));
        }
        Ok(())
    }
    fn track(&self, id: u16) -> Result<&Track, Error> {
        self.tracks
            .iter()
            .find(|track| track.id == id)
            .ok_or(Error::Protocol("unknown track"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Key,
    Reference,
    Disposable,
    Audio,
}
impl FrameKind {
    fn independent(self) -> bool {
        matches!(self, Self::Key | Self::Audio)
    }
    fn tag(self) -> u8 {
        match self {
            Self::Key => 0,
            Self::Reference => 1,
            Self::Disposable => 2,
            Self::Audio => 3,
        }
    }
}

/// One media unit with explicit, same-track, backward dependencies.
/// 单个媒体单元，依赖必须指向同轨道更早的单元；帧号在整个会话内唯一。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub track: u16,
    pub sequence: u64,
    pub timestamp_us: u64,
    pub duration_us: u32,
    pub kind: FrameKind,
    pub dependencies: Vec<u64>,
    pub payload: Vec<u8>,
}
impl Frame {
    pub(crate) fn validate(&self, catalog: &Catalog) -> Result<(), Error> {
        let track = catalog.track(self.track)?;
        if self.sequence == 0
            || self.duration_us == 0
            || self.duration_us > 1_000_000
            || self.payload.is_empty()
            || self.payload.len() > MAX_OBJECT_BYTES - 68
            || self.dependencies.len() > MAX_DEPENDENCIES
        {
            return Err(Error::Protocol("media frame bounds"));
        }
        if self.kind.independent() != self.dependencies.is_empty()
            || (self.kind == FrameKind::Audio) != (track.kind == TrackKind::Audio)
        {
            return Err(Error::Protocol("media frame kind"));
        }
        let mut ids = HashSet::new();
        if self
            .dependencies
            .iter()
            .any(|id| *id == 0 || *id >= self.sequence || !ids.insert(*id))
        {
            return Err(Error::Protocol("media dependencies"));
        }
        Ok(())
    }
    fn object(&self, catalog: &Catalog) -> Result<Object, Error> {
        self.validate(catalog)?;
        let mut payload = vec![self.kind.tag()];
        payload.extend_from_slice(&self.track.to_be_bytes());
        payload.push(self.dependencies.len() as u8);
        for id in &self.dependencies {
            payload.extend_from_slice(&id.to_be_bytes());
        }
        payload.extend_from_slice(&self.payload);
        Ok(Object {
            sequence: self.sequence,
            timestamp_us: self.timestamp_us,
            duration_us: self.duration_us,
            independent: self.kind.independent(),
            payload,
        })
    }
    fn from_object(object: Object, catalog: &Catalog) -> Result<Self, Error> {
        let bytes = &object.payload;
        if bytes.len() < 4 || bytes[3] as usize > MAX_DEPENDENCIES {
            return Err(Error::Protocol("media header"));
        }
        let kind = match bytes[0] {
            0 => FrameKind::Key,
            1 => FrameKind::Reference,
            2 => FrameKind::Disposable,
            3 => FrameKind::Audio,
            _ => return Err(Error::Protocol("media kind")),
        };
        let header = 4 + bytes[3] as usize * 8;
        if bytes.len() <= header || object.independent != kind.independent() {
            return Err(Error::Protocol("media dependency header"));
        }
        let dependencies = bytes[4..header]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|bytes| u64::from_be_bytes(*bytes))
            .collect();
        let frame = Self {
            track: u16::from_be_bytes([bytes[1], bytes[2]]),
            sequence: object.sequence,
            timestamp_us: object.timestamp_us,
            duration_us: object.duration_us,
            kind,
            dependencies,
            payload: bytes[header..].to_vec(),
        };
        frame.validate(catalog)?;
        Ok(frame)
    }
}

/// Publisher for the PERI media-01 payload profile.
/// PERI media-01 负载配置发布端，目录必须先于媒体单元发送。
pub struct Publisher {
    session: Session,
    catalog: Catalog,
}
impl Publisher {
    /// Send the immutable catalog before accepting media sends.
    /// 先发送不可变目录，再开放媒体发送接口。
    pub async fn new(session: Session, catalog: Catalog, budget: Duration) -> Result<Self, Error> {
        catalog.validate(
            session
                .agreement()
                .ok_or(Error::Protocol("closed session"))?,
        )?;
        let mut payload = CATALOG_MAGIC.to_vec();
        payload.extend_from_slice(&serde_json::to_vec(&catalog)?);
        session
            .send(
                &Object {
                    sequence: 0,
                    timestamp_us: 0,
                    duration_us: 0,
                    independent: true,
                    payload,
                },
                budget,
            )
            .await?;
        Ok(Self { session, catalog })
    }
    /// Validate and transmit one media frame within a local I/O budget.
    /// 校验媒体单元并在本地 I/O 预算内发送。
    pub async fn send(&self, frame: &Frame, budget: Duration) -> Result<(), Error> {
        self.session
            .send(&frame.object(&self.catalog)?, budget)
            .await
    }
    pub(crate) async fn start_send(
        &self,
        frame: &Frame,
        budget: Duration,
    ) -> Result<crate::ObjectSend, Error> {
        self.session
            .start_send(&frame.object(&self.catalog)?, budget)
            .await
    }
}

/// Subscriber that validates the initial catalog and every encoded media unit.
/// 订阅端校验初始目录和每个媒体单元，不将网络收取当作解码成功。
pub struct Subscriber {
    session: Session,
    catalog: Catalog,
}
impl Subscriber {
    /// Read and validate the mandatory catalog before receiving media.
    /// 接收媒体前读取并校验必需的目录。
    pub async fn new(mut session: Session, budget: Duration) -> Result<Self, Error> {
        let object = session.recv(budget).await?;
        if object.sequence != 0
            || object.timestamp_us != 0
            || object.duration_us != 0
            || !object.independent
            || object.payload.len() > MAX_CATALOG
            || !object.payload.starts_with(CATALOG_MAGIC)
        {
            return Err(Error::Protocol("expected media catalog"));
        }
        let catalog: Catalog = serde_json::from_slice(&object.payload[CATALOG_MAGIC.len()..])?;
        catalog.validate(
            session
                .agreement()
                .ok_or(Error::Protocol("closed session"))?,
        )?;
        Ok(Self { session, catalog })
    }
    /// Return the validated immutable track catalog.
    /// 返回已校验的不可变轨道目录。
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    /// Receive a validated frame; decoding and deadline scheduling are separate.
    /// 接收已校验的媒体单元，解码及截止时间调度由调用方驱动。
    pub async fn recv(&mut self, budget: Duration) -> Result<Frame, Error> {
        Frame::from_object(self.session.recv(budget).await?, &self.catalog)
    }
    /// Receive live media, reporting local expiry or RESET(1) as a recovery gap.
    pub async fn recv_live(
        &mut self,
        idle: Duration,
        residence: Duration,
    ) -> Result<Option<Frame>, Error> {
        self.session
            .recv_live(idle, residence)
            .await?
            .map(|object| Frame::from_object(object, &self.catalog))
            .transpose()
    }
}

struct Pending {
    frame: Frame,
    deadline: Instant,
}
struct Inflight {
    track: u16,
    kind: FrameKind,
    deadline: Instant,
}

/// A local decode decision; Recover asks the application to obtain an independent unit.
/// 本地解码决策；Recover 要求调用方获取独立单元，不自动发送网络反馈。
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Decode(Frame),
    Drop { sequence: u64 },
    Recover { track: u16 },
}

/// Bounded buffer that gates dependencies on explicit decoder confirmation.
/// 有界缓冲区；依赖仅在实际解码确认后可用，截止时间使用本地 Instant。
pub struct DecodeBuffer {
    catalog: Catalog,
    pending: Vec<Pending>,
    inflight: HashMap<u64, Inflight>,
    decoded: VecDeque<(u64, u16)>,
    seen: VecDeque<u64>,
    highest: u64,
    bytes: usize,
    max_bytes: usize,
    max_frames: usize,
    max_age: Duration,
}
impl DecodeBuffer {
    /// Configure bounded queued bytes, total frame slots and negotiated residence.
    /// 设置排队字节、排队及解码中的总帧数与协商驻留上限。
    pub fn new(
        catalog: Catalog,
        agreement: &Agreement,
        max_bytes: usize,
        max_frames: usize,
    ) -> Result<Self, Error> {
        catalog.validate(agreement)?;
        if max_bytes == 0 || max_frames == 0 || max_frames > 128 || agreement.buffer.maximum_ms == 0
        {
            return Err(Error::Protocol("decode buffer bounds"));
        }
        Ok(Self {
            catalog,
            pending: vec![],
            inflight: HashMap::new(),
            decoded: VecDeque::new(),
            seen: VecDeque::new(),
            highest: 0,
            bytes: 0,
            max_bytes,
            max_frames,
            max_age: Duration::from_millis(agreement.buffer.maximum_ms.into()),
        })
    }
    /// Insert with a playback deadline clamped to the negotiated maximum residence.
    /// 插入并将播放截止时间限制在协商的最大驻留窗口内；不使用远端墙钟。
    pub fn push(&mut self, frame: Frame, now: Instant, deadline: Instant) -> Result<(), Error> {
        frame.validate(&self.catalog)?;
        if (self.highest >= MAX_HISTORY as u64
            && frame.sequence <= self.highest - MAX_HISTORY as u64)
            || self.seen.contains(&frame.sequence)
            || self
                .pending
                .iter()
                .any(|entry| entry.frame.sequence == frame.sequence)
            || self.inflight.contains_key(&frame.sequence)
        {
            return Err(Error::Protocol("duplicate media sequence"));
        }
        let total = self
            .bytes
            .checked_add(frame.payload.len())
            .ok_or(Error::Protocol("decode buffer full"))?;
        if self.pending.len() + self.inflight.len() >= self.max_frames || total > self.max_bytes {
            return Err(Error::Protocol("decode buffer full"));
        }
        for dependency in &frame.dependencies {
            let track = self
                .pending
                .iter()
                .find(|entry| entry.frame.sequence == *dependency)
                .map(|entry| entry.frame.track)
                .or_else(|| self.inflight.get(dependency).map(|entry| entry.track))
                .or_else(|| {
                    self.decoded
                        .iter()
                        .find(|(id, _)| id == dependency)
                        .map(|(_, track)| *track)
                });
            if track.is_some_and(|track| track != frame.track) {
                return Err(Error::Protocol("cross-track dependency"));
            }
        }
        if self.pending.iter().any(|entry| {
            entry.frame.track != frame.track && entry.frame.dependencies.contains(&frame.sequence)
        }) {
            return Err(Error::Protocol("cross-track dependency"));
        }
        self.highest = self.highest.max(frame.sequence);
        self.seen.push_back(frame.sequence);
        self.seen
            .retain(|id| *id > self.highest.saturating_sub(MAX_HISTORY as u64));
        self.bytes = total;
        self.pending.push(Pending {
            frame,
            deadline: deadline.min(now + self.max_age),
        });
        Ok(())
    }
    /// Expire late work first, then choose a dependency-ready frame by deadline.
    /// 先处理过期单元，再按截止时间选取依赖已解码的帧。
    pub fn next(&mut self, now: Instant) -> Option<Event> {
        if let Some((&id, state)) = self
            .inflight
            .iter()
            .find(|(_, state)| state.deadline <= now)
        {
            let track = state.track;
            let disposable = state.kind == FrameKind::Disposable;
            self.inflight.remove(&id);
            return Some(if disposable {
                Event::Drop { sequence: id }
            } else {
                self.recover(track);
                Event::Recover { track }
            });
        }
        if let Some(index) = self.pending.iter().position(|entry| entry.deadline <= now) {
            let frame = self.take(index).frame;
            return Some(if frame.kind == FrameKind::Disposable {
                Event::Drop {
                    sequence: frame.sequence,
                }
            } else {
                self.recover(frame.track);
                Event::Recover { track: frame.track }
            });
        }
        let index = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .frame
                    .dependencies
                    .iter()
                    .all(|id| self.decoded.contains(&(*id, entry.frame.track)))
            })
            .min_by_key(|(_, entry)| entry.deadline)
            .map(|(index, _)| index)?;
        let entry = self.take(index);
        self.inflight.insert(
            entry.frame.sequence,
            Inflight {
                track: entry.frame.track,
                kind: entry.frame.kind,
                deadline: entry.deadline,
            },
        );
        Some(Event::Decode(entry.frame))
    }
    /// Confirm decoder success before dependents may run; late confirmations are rejected.
    /// 实际解码成功后确认；过期确认失败并清理受影响轨道。
    pub fn decoded(&mut self, sequence: u64, now: Instant) -> Result<(), Error> {
        let state = self
            .inflight
            .remove(&sequence)
            .ok_or(Error::Protocol("unknown decode confirmation"))?;
        if state.deadline <= now {
            if state.kind != FrameKind::Disposable {
                self.recover(state.track);
            }
            return Err(Error::Protocol("late decode confirmation"));
        }
        if state.kind != FrameKind::Disposable {
            self.decoded.push_back((sequence, state.track));
            if self.decoded.len() > MAX_HISTORY {
                self.decoded.pop_front();
            }
        }
        Ok(())
    }
    /// Invalidate a failed track and retain queued independent recovery units.
    /// 解码失败时清空轨道依赖状态，仅保留排队的独立恢复单元。
    pub fn recover(&mut self, track: u16) {
        self.decoded.retain(|(_, id)| *id != track);
        self.inflight.retain(|_, state| state.track != track);
        self.pending
            .retain(|entry| entry.frame.track != track || entry.frame.kind.independent());
        self.bytes = self
            .pending
            .iter()
            .map(|entry| entry.frame.payload.len())
            .sum();
    }
    /// Count queued encoded payload bytes, excluding decoder-owned frames.
    /// 统计排队编码负载字节，不含已交给解码器的帧。
    pub fn queued_bytes(&self) -> usize {
        self.bytes
    }
    fn take(&mut self, index: usize) -> Pending {
        let entry = self.pending.remove(index);
        self.bytes -= entry.frame.payload.len();
        entry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_types::{Hello, PERI_VERSION, Profile};
    fn fixture() -> (Catalog, Agreement) {
        let hello = Hello {
            version: PERI_VERSION,
            profiles: vec![Profile::Live],
            codecs: vec!["video/av1".into()],
            extensions: vec![],
        };
        let agreement = peri_session::negotiate(&hello, &hello).unwrap();
        let catalog = Catalog {
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                codec: "video/av1".into(),
                init: vec![0x81, 0x00],
                width: Some(1920),
                height: Some(1080),
                sample_rate: None,
                channels: None,
            }],
        };
        (catalog, agreement)
    }
    #[test]
    fn inflight_slots_are_bounded_and_late_disposable_does_not_break_reference() {
        let (catalog, agreement) = fixture();
        let mut buffer = DecodeBuffer::new(catalog, &agreement, 20, 1).unwrap();
        let now = Instant::now();
        let deadline = now + Duration::from_millis(100);
        buffer
            .push(frame(1, FrameKind::Key, vec![]), now, deadline)
            .unwrap();
        buffer.next(now);
        assert!(
            buffer
                .push(frame(2, FrameKind::Disposable, vec![1]), now, deadline)
                .is_err()
        );
        buffer.decoded(1, now).unwrap();
        buffer
            .push(frame(2, FrameKind::Disposable, vec![1]), now, deadline)
            .unwrap();
        buffer.next(now);
        let later = now + Duration::from_millis(100);
        assert!(buffer.decoded(2, later).is_err());
        buffer
            .push(
                frame(3, FrameKind::Reference, vec![1]),
                later,
                later + Duration::from_millis(100),
            )
            .unwrap();
        assert!(matches!(
            buffer.next(later),
            Some(Event::Decode(Frame { sequence: 3, .. }))
        ));
    }
    fn frame(sequence: u64, kind: FrameKind, dependencies: Vec<u64>) -> Frame {
        Frame {
            track: 1,
            sequence,
            timestamp_us: sequence * 33333,
            duration_us: 33333,
            kind,
            dependencies,
            payload: vec![42; 10],
        }
    }
    #[test]
    fn catalog_rejects_duplicate_ids_wrong_codec_and_invalid_properties() {
        let (catalog, agreement) = fixture();
        catalog.validate(&agreement).unwrap();
        for scenario in 0..5 {
            let mut invalid = catalog.clone();
            match scenario {
                0 => invalid.tracks.push(invalid.tracks[0].clone()),
                1 => invalid.tracks[0].codec = "video/vp9".into(),
                2 => invalid.tracks[0].width = Some(0),
                3 => invalid.tracks[0].channels = Some(2),
                _ => invalid.tracks[0].init = vec![0; 4097],
            }
            assert!(invalid.validate(&agreement).is_err());
        }
    }
    #[test]
    fn frame_profile_round_trip_and_malformed_metadata_rejection() {
        let (catalog, _) = fixture();
        let media = frame(3, FrameKind::Reference, vec![1, 2]);
        assert_eq!(
            Frame::from_object(media.object(&catalog).unwrap(), &catalog).unwrap(),
            media
        );
        for scenario in 0..5 {
            let mut invalid = media.clone();
            match scenario {
                0 => invalid.dependencies = vec![3],
                1 => invalid.dependencies = vec![1, 1],
                2 => invalid.track = 99,
                3 => invalid.kind = FrameKind::Key,
                _ => invalid.duration_us = 0,
            }
            assert!(invalid.object(&catalog).is_err());
        }
        let mut object = media.object(&catalog).unwrap();
        object.payload[3] = 9;
        assert!(Frame::from_object(object, &catalog).is_err());
    }
    #[test]
    fn receipt_is_not_decoder_confirmation_and_reorder_waits_for_dependency() {
        let (catalog, agreement) = fixture();
        let mut buffer = DecodeBuffer::new(catalog, &agreement, 1000, 8).unwrap();
        let now = Instant::now();
        let deadline = now + Duration::from_millis(100);
        buffer
            .push(frame(2, FrameKind::Reference, vec![1]), now, deadline)
            .unwrap();
        assert!(buffer.next(now).is_none());
        let key = frame(1, FrameKind::Key, vec![]);
        buffer.push(key.clone(), now, deadline).unwrap();
        assert_eq!(buffer.next(now), Some(Event::Decode(key)));
        assert!(buffer.next(now).is_none());
        buffer.decoded(1, now).unwrap();
        assert!(matches!(
            buffer.next(now),
            Some(Event::Decode(Frame { sequence: 2, .. }))
        ));
        assert!(
            buffer
                .push(frame(1, FrameKind::Key, vec![]), now, deadline)
                .is_err()
        );
    }
    #[test]
    fn expiry_recovers_reference_chain_but_keeps_independent_recovery_frame() {
        let (catalog, agreement) = fixture();
        let mut buffer = DecodeBuffer::new(catalog, &agreement, 1000, 8).unwrap();
        let now = Instant::now();
        let later = now + Duration::from_millis(100);
        buffer
            .push(frame(1, FrameKind::Reference, vec![]), now, later)
            .unwrap_err();
        buffer
            .push(frame(2, FrameKind::Reference, vec![1]), now, now)
            .unwrap();
        buffer
            .push(frame(3, FrameKind::Reference, vec![2]), now, later)
            .unwrap();
        buffer
            .push(frame(4, FrameKind::Key, vec![]), now, later)
            .unwrap();
        assert_eq!(buffer.next(now), Some(Event::Recover { track: 1 }));
        assert_eq!(buffer.queued_bytes(), 10);
        assert!(matches!(
            buffer.next(now),
            Some(Event::Decode(Frame { sequence: 4, .. }))
        ));
        assert!(buffer.decoded(2, now).is_err());
        assert!(
            buffer
                .push(frame(3, FrameKind::Reference, vec![2]), now, later)
                .is_err()
        );
    }
    #[test]
    fn negotiated_residence_limits_deadlines_and_late_decoder_confirmation() {
        let (catalog, agreement) = fixture();
        let mut buffer = DecodeBuffer::new(catalog, &agreement, 1000, 8).unwrap();
        let now = Instant::now();
        buffer
            .push(
                frame(1, FrameKind::Key, vec![]),
                now,
                now + Duration::from_secs(60),
            )
            .unwrap();
        assert!(matches!(buffer.next(now), Some(Event::Decode(_))));
        let expired = now + Duration::from_millis(agreement.buffer.maximum_ms.into());
        assert!(buffer.decoded(1, expired).is_err());
        buffer
            .push(
                frame(2, FrameKind::Reference, vec![1]),
                expired,
                expired + Duration::from_millis(100),
            )
            .unwrap();
        assert!(buffer.next(expired).is_none());
    }
    #[test]
    fn disposable_expiry_preserves_decoded_reference_and_overflow_is_atomic() {
        let (catalog, agreement) = fixture();
        let mut buffer = DecodeBuffer::new(catalog, &agreement, 20, 2).unwrap();
        let now = Instant::now();
        let deadline = now + Duration::from_millis(100);
        buffer
            .push(frame(1, FrameKind::Key, vec![]), now, deadline)
            .unwrap();
        buffer.next(now);
        buffer.decoded(1, now).unwrap();
        buffer
            .push(frame(2, FrameKind::Disposable, vec![1]), now, now)
            .unwrap();
        buffer
            .push(frame(3, FrameKind::Reference, vec![1]), now, deadline)
            .unwrap();
        assert!(
            buffer
                .push(frame(4, FrameKind::Reference, vec![3]), now, deadline)
                .is_err()
        );
        assert_eq!(buffer.queued_bytes(), 20);
        assert_eq!(buffer.next(now), Some(Event::Drop { sequence: 2 }));
        assert!(matches!(
            buffer.next(now),
            Some(Event::Decode(Frame { sequence: 3, .. }))
        ));
    }
    #[test]
    fn cross_track_dependencies_and_old_replays_are_rejected() {
        let (mut catalog, agreement) = fixture();
        let mut second = catalog.tracks[0].clone();
        second.id = 2;
        catalog.tracks.push(second);
        let mut buffer = DecodeBuffer::new(catalog, &agreement, 1000, 8).unwrap();
        let now = Instant::now();
        let deadline = now + Duration::from_millis(100);
        buffer
            .push(frame(2, FrameKind::Reference, vec![1]), now, deadline)
            .unwrap();
        let mut cross = frame(1, FrameKind::Key, vec![]);
        cross.track = 2;
        assert!(buffer.push(cross, now, deadline).is_err());
        buffer
            .push(frame(200, FrameKind::Key, vec![]), now, deadline)
            .unwrap();
        assert!(
            buffer
                .push(frame(1, FrameKind::Key, vec![]), now, deadline)
                .is_err()
        );
    }
}
