//! Bounded AV1 IVF adapters with decoder-probed GOP support.
//! 有界 AV1 IVF 适配器；GOP 模式使用外部解码探测与保守顺序依赖。
use crate::media::{Catalog, Frame, FrameKind, Track, TrackKind};

/// Read an explicitly declared all-intra AV1 clip without re-encoding.
/// 读取调用方明确声明为全关键帧的 AV1 片段，不重新编码；此约束不由比特流解析验证。
pub fn read_all_intra_ivf(bytes: &[u8]) -> Result<(Catalog, Vec<Frame>), crate::Error> {
    if bytes.len() < 32
        || bytes.len() > 16 * 1024 * 1024
        || &bytes[..4] != b"DKIF"
        || &bytes[8..12] != b"AV01"
        || bytes[4..8] != [0, 0, 32, 0]
    {
        return Err(crate::Error::Protocol("invalid generated AV1 IVF"));
    }
    let width = u16::from_le_bytes(bytes[12..14].try_into().expect("validated IVF field"));
    let height = u16::from_le_bytes(bytes[14..16].try_into().expect("validated IVF field"));
    let rate = u32::from_le_bytes(bytes[16..20].try_into().expect("validated IVF field"));
    let scale = u32::from_le_bytes(bytes[20..24].try_into().expect("validated IVF field"));
    if width == 0
        || width > 8192
        || height > 8192
        || height == 0
        || rate == 0
        || scale == 0
        || scale > rate
    {
        return Err(crate::Error::Protocol("invalid IVF timing"));
    }
    let mut frames: Vec<Frame> = Vec::new();
    let mut offset = 32;
    while offset < bytes.len() {
        if offset + 12 > bytes.len() {
            return Err(crate::Error::Protocol("truncated IVF header"));
        }
        let size = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("validated IVF field"),
        ) as usize;
        let ticks = u64::from_le_bytes(
            bytes[offset + 4..offset + 12]
                .try_into()
                .expect("validated IVF field"),
        );
        offset += 12;
        if size == 0
            || size > 1024 * 1024 - 68
            || offset + size > bytes.len()
            || frames.len() >= 120
        {
            return Err(crate::Error::Protocol("invalid IVF frame bounds"));
        }
        let timestamp =
            u64::try_from(u128::from(ticks) * u128::from(scale) * 1_000_000 / u128::from(rate))
                .map_err(|_| crate::Error::Protocol("IVF timestamp overflow"))?;
        if frames
            .last()
            .is_some_and(|previous| previous.timestamp_us >= timestamp)
        {
            return Err(crate::Error::Protocol("invalid IVF order"));
        }
        // 调用方声明全关键帧；此处不尝试从任意 AV1 比特流猜测依赖。
        frames.push(Frame {
            track: 1,
            sequence: frames.len() as u64 + 1,
            timestamp_us: timestamp,
            duration_us: (u64::from(scale) * 1_000_000 / u64::from(rate)) as u32,
            kind: FrameKind::Key,
            dependencies: vec![],
            payload: bytes[offset..offset + size].to_vec(),
        });
        offset += size;
    }
    if frames.is_empty() {
        return Err(crate::Error::Protocol("empty generated IVF"));
    }
    let origin = frames[0].timestamp_us;
    for frame in &mut frames {
        frame.timestamp_us -= origin;
    }
    if frames.last().expect("nonempty").timestamp_us > 120_000_000
        || frames
            .windows(2)
            .any(|pair| pair[1].timestamp_us - pair[0].timestamp_us > 1_000_000)
    {
        return Err(crate::Error::Protocol("IVF timeline bounds"));
    }
    // IVF 的 rate/scale 表示时间基准，不一定等于帧率；微秒时间基准也必须可读。
    for index in 0..frames.len() {
        let duration = if index + 1 < frames.len() {
            frames[index + 1].timestamp_us - frames[index].timestamp_us
        } else if index > 0 {
            u64::from(frames[index - 1].duration_us)
        } else {
            let nominal = u64::from(scale) * 1_000_000 / u64::from(rate);
            if (8333..=1_000_000).contains(&nominal) {
                nominal
            } else {
                33333
            }
        };
        if !(8333..=1_000_000).contains(&duration) {
            return Err(crate::Error::Protocol("IVF frame interval"));
        }
        frames[index].duration_us = duration as u32;
    }
    Ok((
        Catalog {
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                codec: "video/av1".into(),
                init: vec![],
                width: Some(width),
                height: Some(height),
                sample_rate: None,
                channels: None,
            }],
        },
        frames,
    ))
}

/// Validate decoder-probed, one-visible-frame-per-packet AV1 and use a conservative
/// decode-order dependency chain. This is not an AV1 reference-slot parser.
pub fn read_gop_ivf(bytes: &[u8], probe: &[u8]) -> Result<(Catalog, Vec<Frame>), crate::Error> {
    #[derive(serde::Deserialize)]
    struct Probe {
        frames: Vec<Info>,
    }
    #[derive(serde::Deserialize)]
    struct Info {
        key_frame: u8,
        pts: u64,
        pkt_size: String,
        width: u16,
        height: u16,
    }
    if probe.len() > 256 * 1024 {
        return Err(crate::Error::Protocol("probe limit"));
    }
    let info: Probe = serde_json::from_slice(probe)?;
    let (catalog, mut frames) = read_all_intra_ivf(bytes)?;
    if info.frames.len() != frames.len() || info.frames.first().is_none_or(|f| f.key_frame != 1) {
        return Err(crate::Error::Protocol(
            "expected one displayed frame per IVF packet",
        ));
    }
    let mut offset = 32;
    for (frame, info) in frames.iter_mut().zip(info.frames) {
        let ticks = u64::from_le_bytes(
            bytes[offset + 4..offset + 12]
                .try_into()
                .expect("IVF checked"),
        );
        if info.key_frame > 1
            || info.pts != ticks
            || info.pkt_size.parse::<usize>().ok() != Some(frame.payload.len())
            || Some(info.width) != catalog.tracks[0].width
            || Some(info.height) != catalog.tracks[0].height
        {
            return Err(crate::Error::Protocol(
                "unsupported reordered or changing AV1 profile",
            ));
        }
        offset += 12 + frame.payload.len();
        if info.key_frame == 0 {
            frame.kind = FrameKind::Reference;
            frame.dependencies = vec![frame.sequence - 1];
        }
    }
    Ok((catalog, frames))
}

/// Streaming writer for an AV1 track; the recording must start at a keyframe.
/// 单条 AV1 轨道的有界流式录制，首帧必须为关键帧。
pub struct IvfWriter<W> {
    writer: W,
    track: Track,
    count: u32,
    bytes: usize,
    timestamp: Option<u64>,
    references: std::collections::VecDeque<u64>,
    sequence: u64,
}
impl<W: std::io::Write + std::io::Seek> IvfWriter<W> {
    /// Write an IVF header for an explicitly selected AV1 video track.
    /// 为明确选择的 AV1 视频轨道写入 IVF 文件头。
    pub fn new(mut writer: W, track: Track) -> Result<Self, crate::Error> {
        if track.codec != "video/av1"
            || track.kind != TrackKind::Video
            || track.id == 0
            || !track.width.is_some_and(|width| (1..=8192).contains(&width))
            || !track
                .height
                .is_some_and(|height| (1..=8192).contains(&height))
            || track.sample_rate.is_some()
            || track.channels.is_some()
        {
            return Err(crate::Error::Protocol("expected AV1 video track"));
        }
        let mut header = b"DKIF\x00\x00\x20\x00AV01".to_vec();
        header.extend_from_slice(
            &track
                .width
                .ok_or(crate::Error::Protocol("video width"))?
                .to_le_bytes(),
        );
        header.extend_from_slice(
            &track
                .height
                .ok_or(crate::Error::Protocol("video height"))?
                .to_le_bytes(),
        );
        header.extend_from_slice(&1_000_000_u32.to_le_bytes());
        header.extend_from_slice(&1_u32.to_le_bytes());
        header.extend_from_slice(&[0; 8]);
        writer.write_all(&header)?;
        Ok(Self {
            writer,
            track,
            count: 0,
            bytes: 0,
            timestamp: None,
            references: std::collections::VecDeque::new(),
            sequence: 0,
        })
    }
    /// Append an AV1 unit with recorded dependencies, rejecting timeline reversals.
    /// 追加依赖已写入的 AV1 单元，拒绝时间轴回退和过大的录制文件。
    pub fn push(&mut self, frame: &Frame) -> Result<(), crate::Error> {
        frame.validate(&Catalog {
            tracks: vec![self.track.clone()],
        })?;
        let bytes = self
            .bytes
            .checked_add(frame.payload.len())
            .ok_or(crate::Error::Protocol("recording limit"))?;
        if (self.count == 0 && frame.kind != FrameKind::Key)
            || frame.sequence <= self.sequence
            || frame
                .dependencies
                .iter()
                .any(|id| !self.references.contains(id))
            || self.count >= 10000
            || bytes > 128 * 1024 * 1024
            || self
                .timestamp
                .is_some_and(|time| time >= frame.timestamp_us)
        {
            return Err(crate::Error::Protocol("IVF recording bounds"));
        }
        self.writer
            .write_all(&(frame.payload.len() as u32).to_le_bytes())?;
        self.writer.write_all(&frame.timestamp_us.to_le_bytes())?;
        self.writer.write_all(&frame.payload)?;
        self.timestamp = Some(frame.timestamp_us);
        self.count += 1;
        self.sequence = frame.sequence;
        if frame.kind != FrameKind::Disposable {
            self.references.push_back(frame.sequence);
            if self.references.len() > 128 {
                self.references.pop_front();
            }
        }
        self.bytes = bytes;
        Ok(())
    }
    /// Finalize the frame count and return the underlying writer.
    /// 更新帧计数并返回底层写入器。
    pub fn finish(mut self) -> Result<W, crate::Error> {
        self.writer.seek(std::io::SeekFrom::Start(24))?;
        self.writer.write_all(&self.count.to_le_bytes())?;
        self.writer.flush()?;
        Ok(self.writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn track() -> Track {
        Track {
            id: 1,
            kind: TrackKind::Video,
            codec: "video/av1".into(),
            init: vec![],
            width: Some(320),
            height: Some(180),
            sample_rate: None,
            channels: None,
        }
    }
    #[test]
    fn gop_probe_requires_exact_packets_and_key_start() {
        let mut writer = IvfWriter::new(Cursor::new(Vec::new()), track()).unwrap();
        let key = Frame {
            track: 1,
            sequence: 1,
            timestamp_us: 0,
            duration_us: 33333,
            kind: FrameKind::Key,
            dependencies: vec![],
            payload: vec![1],
        };
        let mut delta = key.clone();
        delta.sequence = 2;
        delta.timestamp_us = 33333;
        delta.kind = FrameKind::Reference;
        delta.dependencies = vec![1];
        assert!(writer.push(&delta).is_err());
        writer.push(&key).unwrap();
        writer.push(&delta).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let mut probe = serde_json::json!({"frames":[
            {"key_frame":1,"pts":0,"pkt_size":"1","width":320,"height":180},
            {"key_frame":0,"pts":33333,"pkt_size":"1","width":320,"height":180}
        ]});
        let (_, frames) = read_gop_ivf(&bytes, &serde_json::to_vec(&probe).unwrap()).unwrap();
        assert_eq!(frames, [key, delta]);
        for field in ["pts", "key_frame", "width", "height"] {
            let mut invalid = probe.clone();
            invalid["frames"][0][field] = serde_json::json!(99);
            assert!(read_gop_ivf(&bytes, &serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        probe["frames"].as_array_mut().unwrap().pop();
        assert!(read_gop_ivf(&bytes, &serde_json::to_vec(&probe).unwrap()).is_err());
    }

    #[test]
    fn writer_preserves_units_and_rejects_timeline_reversal() {
        let mut writer = IvfWriter::new(Cursor::new(Vec::new()), track()).unwrap();
        let first = Frame {
            track: 1,
            sequence: 1,
            timestamp_us: 0,
            duration_us: 33333,
            kind: FrameKind::Key,
            dependencies: vec![],
            payload: vec![1, 2, 3],
        };
        writer.push(&first).unwrap();
        assert!(writer.push(&first).is_err());
        let bytes = writer.finish().unwrap().into_inner();
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 1);
        let (catalog, frames) = read_all_intra_ivf(&bytes).unwrap();
        assert_eq!(catalog.tracks[0], track());
        assert_eq!(frames, [first]);
    }
    #[test]
    fn parser_rejects_declared_size_and_truncated_payload_without_allocating_it() {
        let mut writer = IvfWriter::new(Cursor::new(Vec::new()), track()).unwrap();
        writer
            .push(&Frame {
                track: 1,
                sequence: 1,
                timestamp_us: 0,
                duration_us: 33333,
                kind: FrameKind::Key,
                dependencies: vec![],
                payload: vec![42],
            })
            .unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        assert!(read_all_intra_ivf(&bytes[..bytes.len() - 1]).is_err());
        bytes[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_all_intra_ivf(&bytes).is_err());
    }
}
