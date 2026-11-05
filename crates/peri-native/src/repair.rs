//! 可选对象修复内核：有界分片、单丢片异或恢复、完整性验证和冗余字节预算。
//! 不自动读取QUIC流、不宣称解码成功；Relay与正式播放器尚未接入此负载配置。
use crate::{Error, Object, wire};
use peri_types::{Agreement, Extension};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub const EXTENSION_ID: &str = "peri.repair";
const MAGIC: &[u8; 8] = b"PRPR\x01\x00\x00\x00";
pub const HEADER_BYTES: usize = 56;
pub const MAX_SHARDS: usize = 32;
pub const MAX_SHARD_BYTES: usize = 1024;
const HISTORY: usize = 128;

/// 调用方只有准备使用本模块的专用适配器时才应提供此能力；默认Hello不启用。
pub fn capability(required: bool) -> Extension {
    Extension {
        id: EXTENSION_ID.into(),
        version: 1,
        required,
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    // 复用现有TLS后端的公开SHA-256提供器，不增加依赖；这里不派生密钥。
    rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256
        .tls13()
        .expect("fixed TLS13 suite")
        .common
        .hash_provider
        .hash(bytes)
        .as_ref()
        .try_into()
        .expect("SHA256 output")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    group: u64,
    len: usize,
    count: usize,
    size: usize,
    hash: [u8; 32],
}
impl Header {
    fn validate(&self) -> Result<(), Error> {
        if self.group == 0
            || !(2..=MAX_SHARDS).contains(&self.count)
            || !(64..=MAX_SHARD_BYTES).contains(&self.size)
            || self.len <= 25
            || self.len > MAX_SHARDS * MAX_SHARD_BYTES
            || self.len.div_ceil(self.size) != self.count
        {
            return Err(Error::Protocol("repair header"));
        }
        Ok(())
    }
    fn reservation(&self) -> usize {
        (self.count + 1) * self.size
    }
}

/// 一个定长数据或校验分片；字段私有，解码时先验证尺寸再复制负载。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    header: Header,
    index: usize,
    payload: Vec<u8>,
}
impl Fragment {
    pub fn group(&self) -> u64 {
        self.header.group
    }
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn is_parity(&self) -> bool {
        self.index == self.header.count
    }
    pub fn encoded_len(&self) -> usize {
        HEADER_BYTES + self.payload.len()
    }
    /// 全组相对原对象新增的原生字节：校验、全部分片头、补齐及外层对象头；不含QUIC开销。
    pub fn protection_overhead(&self) -> usize {
        (self.header.count + 1) * (self.header.size + HEADER_BYTES + 25) - self.header.len
    }
    fn validate(&self) -> Result<(), Error> {
        self.header.validate()?;
        if self.index > self.header.count || self.payload.len() != self.header.size {
            return Err(Error::Protocol("repair fragment"));
        }
        Ok(())
    }
    /// 固定56字节头：魔数8、组号8、原包长4、数据片数1、片号1、片长2、SHA256为32。
    pub fn encode(&self) -> Vec<u8> {
        let h = &self.header;
        let mut bytes = Vec::with_capacity(self.encoded_len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&h.group.to_be_bytes());
        bytes.extend_from_slice(&(h.len as u32).to_be_bytes());
        bytes.push(h.count as u8);
        bytes.push(self.index as u8);
        bytes.extend_from_slice(&(h.size as u16).to_be_bytes());
        bytes.extend_from_slice(&h.hash);
        bytes.extend_from_slice(&self.payload);
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_BYTES
            || bytes.len() > HEADER_BYTES + MAX_SHARD_BYTES
            || &bytes[..8] != MAGIC
        {
            return Err(Error::Protocol("repair packet"));
        }
        let header = Header {
            group: u64::from_be_bytes(bytes[8..16].try_into().expect("fixed header")),
            len: u32::from_be_bytes(bytes[16..20].try_into().expect("fixed header")) as usize,
            count: bytes[20] as usize,
            size: u16::from_be_bytes(bytes[22..24].try_into().expect("fixed header")) as usize,
            hash: bytes[24..56].try_into().expect("fixed header"),
        };
        header.validate()?;
        let index = bytes[21] as usize;
        if index > header.count || bytes.len() != HEADER_BYTES + header.size {
            return Err(Error::Protocol("repair fragment"));
        }
        Ok(Self {
            header,
            index,
            payload: bytes[HEADER_BYTES..].to_vec(),
        })
    }
    /// 专用适配器可用普通对象流承载分片；分片本身不是可解码媒体，independent必须为false。
    pub fn to_object(&self, wire_sequence: u64) -> Result<Object, Error> {
        if wire_sequence == 0 {
            return Err(Error::Protocol("repair wire sequence"));
        }
        Ok(Object {
            sequence: wire_sequence,
            timestamp_us: 0,
            duration_us: 0,
            independent: false,
            payload: self.encode(),
        })
    }
    pub fn from_object(object: Object) -> Result<Self, Error> {
        if object.sequence == 0
            || object.timestamp_us != 0
            || object.duration_us != 0
            || object.independent
        {
            return Err(Error::Protocol("repair envelope"));
        }
        Self::decode(&object.payload)
    }
}

/// 只能从已经接受修复扩展的Agreement创建；不把本地开关当成双方协商。
#[derive(Clone, Copy)]
pub struct Codec {
    max_age: Duration,
}
impl Codec {
    pub fn new(agreement: &Agreement) -> Result<Self, Error> {
        if agreement.version != peri_types::PERI_VERSION
            || agreement.buffer.maximum_ms == 0
            || agreement.buffer.maximum_ms > 1000
            || !agreement
                .extensions
                .iter()
                .any(|e| e.id == EXTENSION_ID && e.version == 1)
        {
            return Err(Error::Protocol("repair not negotiated"));
        }
        Ok(Self {
            max_age: Duration::from_millis(agreement.buffer.maximum_ms.into()),
        })
    }
    /// 编码完整原生对象包，2至32个数据片加1个校验片；最大原包32KiB。
    pub fn split(&self, object: &Object, shard_bytes: usize) -> Result<Vec<Fragment>, Error> {
        if object.sequence == 0
            || object.duration_us == 0
            || object.payload.len() > MAX_SHARDS * MAX_SHARD_BYTES - 25
            || !(64..=MAX_SHARD_BYTES).contains(&shard_bytes)
        {
            return Err(Error::Protocol("repair object bounds"));
        }
        let packet = object.encode_packet()?;
        let count = packet.len().div_ceil(shard_bytes);
        let header = Header {
            group: object.sequence,
            len: packet.len(),
            count,
            size: shard_bytes,
            hash: digest(&packet),
        };
        header.validate()?;
        let mut fragments = Vec::with_capacity(count + 1);
        let mut parity = vec![0; shard_bytes];
        for (index, chunk) in packet.chunks(shard_bytes).enumerate() {
            let mut payload = vec![0; shard_bytes];
            payload[..chunk.len()].copy_from_slice(chunk);
            for (p, b) in parity.iter_mut().zip(&payload) {
                *p ^= *b;
            }
            fragments.push(Fragment {
                header: header.clone(),
                index,
                payload,
            });
        }
        fragments.push(Fragment {
            header,
            index: count,
            payload: parity,
        });
        Ok(fragments)
    }
    /// 预算不足返回None，调用方应发送原对象；不能改为无校验分片而冒充已保护。
    pub fn protect(
        &self,
        object: &Object,
        shard_bytes: usize,
        budget: &mut RepairBudget,
        now: Instant,
    ) -> Result<Option<Vec<Fragment>>, Error> {
        let fragments = self.split(object, shard_bytes)?;
        if !budget.reserve(fragments.last().expect("parity"), now) {
            return Ok(None);
        }
        Ok(Some(fragments))
    }
    pub fn assembler(&self, limits: Limits) -> Result<Assembler, Error> {
        if limits.groups == 0
            || limits.groups > 8
            || limits.bytes == 0
            || limits.bytes > 256 * 1024
            || limits.max_age.is_zero()
            || limits.max_age > self.max_age
        {
            return Err(Error::Protocol("repair limits"));
        }
        Ok(Assembler {
            groups: Vec::new(),
            reserved: 0,
            seen: VecDeque::new(),
            highest: 0,
            limits,
        })
    }
}

/// 修复额外字节令牌桶，计入全组新增头部和补齐；不包括QUIC/TLS/UDP开销。
pub struct RepairBudget {
    rate: u64,
    capacity: u128,
    credit: u128,
    last: Instant,
}
impl RepairBudget {
    pub fn new(bytes_per_second: u32, burst_bytes: u32, now: Instant) -> Result<Self, Error> {
        if bytes_per_second == 0
            || bytes_per_second > 65536
            || burst_bytes == 0
            || burst_bytes > 65536
        {
            return Err(Error::Protocol("repair budget"));
        }
        let capacity = u128::from(burst_bytes) * 1_000_000_000;
        Ok(Self {
            rate: bytes_per_second.into(),
            capacity,
            credit: capacity,
            last: now,
        })
    }
    pub fn reserve(&mut self, parity: &Fragment, now: Instant) -> bool {
        if !parity.is_parity() {
            return false;
        }
        let Some(elapsed) = now.checked_duration_since(self.last) else {
            return false;
        };
        self.credit = self.capacity.min(
            self.credit
                .saturating_add(elapsed.as_nanos().saturating_mul(self.rate.into())),
        );
        self.last = now;
        let cost = parity.protection_overhead() as u128 * 1_000_000_000;
        if cost > self.credit {
            return false;
        }
        self.credit -= cost;
        true
    }
}

/// 缓存额度预留全部数据和校验负载；不包括调用者输入、少量元数据及重组临时对象。
#[derive(Clone, Copy)]
pub struct Limits {
    pub groups: usize,
    pub bytes: usize,
    pub max_age: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            groups: 4,
            bytes: 128 * 1024,
            max_age: Duration::from_millis(150),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Waiting,
    Duplicate,
    Stale,
    Expired,
    AtCapacity,
    /// repaired只表示补回了一个数据片，不表示已解码或显示。
    Complete {
        object: Object,
        repaired: bool,
    },
}
struct Group {
    header: Header,
    parts: Vec<Option<Vec<u8>>>,
    deadline: Instant,
}
impl Group {
    fn ready(&self) -> bool {
        let data = self.parts[..self.header.count]
            .iter()
            .filter(|p| p.is_some())
            .count();
        data == self.header.count
            || (data + 1 == self.header.count && self.parts[self.header.count].is_some())
    }
    fn finish(mut self) -> Result<(Object, bool), Error> {
        let n = self.header.count;
        let missing = self.parts[..n].iter().position(|p| p.is_none());
        if let Some(index) = missing {
            let mut restored = self.parts[n]
                .take()
                .ok_or(Error::Protocol("repair missing parity"))?;
            for bytes in self.parts[..n].iter().flatten() {
                for (a, b) in restored.iter_mut().zip(bytes) {
                    *a ^= *b;
                }
            }
            self.parts[index] = Some(restored);
        }
        let mut packet = Vec::with_capacity(n * self.header.size);
        for bytes in self.parts[..n].iter().flatten() {
            packet.extend_from_slice(bytes);
        }
        packet.truncate(self.header.len);
        if digest(&packet) != self.header.hash {
            return Err(Error::Protocol("repair integrity"));
        }
        let object = wire::decode_object_packet(&packet)?;
        if object.sequence != self.header.group || object.duration_us == 0 {
            return Err(Error::Protocol("repair group identity"));
        }
        Ok((object, missing.is_some()))
    }
}

/// 有界、传输无关重组窗口；调用者提供单调时间及播放截止时间，不创建后台任务。
pub struct Assembler {
    groups: Vec<Group>,
    reserved: usize,
    seen: VecDeque<u64>,
    highest: u64,
    limits: Limits,
}
impl Assembler {
    pub fn reserved_bytes(&self) -> usize {
        self.reserved
    }
    pub fn pending_groups(&self) -> usize {
        self.groups.len()
    }
    fn remember(&mut self, id: u64) {
        if self.seen.len() == HISTORY {
            self.seen.pop_front();
        }
        self.seen.push_back(id);
    }
    /// 过期组记入有界历史，迟到分片不能重新开启窗口或续命。
    pub fn expire(&mut self, now: Instant) -> usize {
        let mut count = 0;
        let mut index = 0;
        while index < self.groups.len() {
            if self.groups[index].deadline <= now {
                let group = self.groups.remove(index);
                self.reserved -= group.header.reservation();
                self.remember(group.header.group);
                count += 1;
            } else {
                index += 1;
            }
        }
        count
    }
    pub fn push(
        &mut self,
        fragment: Fragment,
        now: Instant,
        deadline: Instant,
    ) -> Result<Admission, Error> {
        fragment.validate()?;
        self.expire(now);
        let id = fragment.group();
        if self.seen.contains(&id) || id <= self.highest.saturating_sub(HISTORY as u64) {
            return Ok(Admission::Stale);
        }
        if deadline <= now {
            return Ok(Admission::Expired);
        }
        let index = match self.groups.iter().position(|g| g.header.group == id) {
            Some(index) => index,
            None => {
                let reservation = fragment.header.reservation();
                if self.groups.len() >= self.limits.groups
                    || reservation > self.limits.bytes - self.reserved
                {
                    return Ok(Admission::AtCapacity);
                }
                self.highest = self.highest.max(id);
                self.reserved += reservation;
                self.groups.push(Group {
                    header: fragment.header.clone(),
                    parts: vec![None; fragment.header.count + 1],
                    deadline: deadline.min(now + self.limits.max_age),
                });
                self.groups.len() - 1
            }
        };
        let group = &mut self.groups[index];
        if group.header != fragment.header {
            return Err(Error::Protocol("repair metadata conflict"));
        }
        if let Some(old) = &group.parts[fragment.index] {
            return if old == &fragment.payload {
                Ok(Admission::Duplicate)
            } else {
                Err(Error::Protocol("repair duplicate conflict"))
            };
        }
        group.parts[fragment.index] = Some(fragment.payload);
        if !group.ready() {
            return Ok(Admission::Waiting);
        }
        let group = self.groups.remove(index);
        self.reserved -= group.header.reservation();
        self.remember(id);
        let (object, repaired) = group.finish()?;
        Ok(Admission::Complete { object, repaired })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn codec() -> Codec {
        let hello = peri_types::Hello {
            version: peri_types::PERI_VERSION,
            profiles: vec![peri_types::Profile::Live],
            codecs: vec!["video/av1".into()],
            extensions: vec![capability(false)],
        };
        Codec::new(&peri_session::negotiate(&hello, &hello).unwrap()).unwrap()
    }
    fn object(sequence: u64, size: usize) -> Object {
        Object {
            sequence,
            timestamp_us: 66667 * sequence,
            duration_us: 66667,
            independent: true,
            payload: (0..size)
                .map(|i| ((i * i + 17 * i + sequence as usize) % 251) as u8)
                .collect(),
        }
    }
    #[test]
    fn every_single_erasure_and_parity_loss_recovers_exact_object() {
        let codec = codec();
        let now = Instant::now();
        for size in [100, 1025, 32743] {
            let original = object(1, size);
            let fragments = codec
                .split(&original, if size == 100 { 64 } else { 1024 })
                .unwrap();
            for omitted in 0..fragments.len() {
                let mut assembler = codec.assembler(Limits::default()).unwrap();
                let mut completed = None;
                for (index, fragment) in fragments.iter().enumerate().rev() {
                    if index == omitted {
                        continue;
                    }
                    let decoded = Fragment::decode(&fragment.encode()).unwrap();
                    if let Admission::Complete { object, repaired } = assembler
                        .push(decoded, now, now + Duration::from_millis(150))
                        .unwrap()
                    {
                        completed = Some(object);
                        assert_eq!(repaired, omitted + 1 < fragments.len());
                    }
                }
                assert_eq!(completed, Some(original.clone()));
                assert_eq!(assembler.reserved_bytes(), 0);
                assert_eq!(
                    assembler
                        .push(
                            fragments[omitted].clone(),
                            now,
                            now + Duration::from_millis(150)
                        )
                        .unwrap(),
                    Admission::Stale
                );
            }
        }
    }
    #[test]
    fn two_missing_shards_wait_until_original_deadline_and_never_revive() {
        let codec = codec();
        let now = Instant::now();
        let fragments = codec.split(&object(1, 1500), 512).unwrap();
        let mut assembler = codec.assembler(Limits::default()).unwrap();
        for fragment in fragments.iter().skip(2) {
            assert_eq!(
                assembler
                    .push(fragment.clone(), now, now + Duration::from_secs(1))
                    .unwrap(),
                Admission::Waiting
            );
        }
        assert_eq!(assembler.expire(now + Duration::from_millis(150)), 1);
        assert_eq!(assembler.reserved_bytes(), 0);
        assert_eq!(
            assembler
                .push(
                    fragments[0].clone(),
                    now + Duration::from_millis(151),
                    now + Duration::from_secs(1)
                )
                .unwrap(),
            Admission::Stale
        );
    }
    #[test]
    fn duplicate_and_conflict_cannot_refresh_or_poison_good_group() {
        let codec = codec();
        let now = Instant::now();
        let fragments = codec.split(&object(1, 1000), 512).unwrap();
        let mut assembler = codec.assembler(Limits::default()).unwrap();
        assert_eq!(
            assembler
                .push(fragments[0].clone(), now, now + Duration::from_millis(100))
                .unwrap(),
            Admission::Waiting
        );
        let reservation = assembler.reserved_bytes();
        assert_eq!(
            assembler
                .push(
                    fragments[0].clone(),
                    now + Duration::from_millis(90),
                    now + Duration::from_secs(1)
                )
                .unwrap(),
            Admission::Duplicate
        );
        let mut conflict = fragments[0].clone();
        conflict.payload[0] ^= 1;
        assert!(
            assembler
                .push(conflict, now, now + Duration::from_millis(100))
                .is_err()
        );
        let mut metadata = fragments[1].clone();
        metadata.header.hash[0] ^= 1;
        assert!(
            assembler
                .push(metadata, now, now + Duration::from_millis(100))
                .is_err()
        );
        assert_eq!(assembler.reserved_bytes(), reservation);
        assert_eq!(assembler.expire(now + Duration::from_millis(100)), 1);
        assert_eq!(assembler.reserved_bytes(), 0);
    }
    #[test]
    fn corrupted_payload_is_not_delivered_and_group_is_retired() {
        let codec = codec();
        let now = Instant::now();
        let mut fragments = codec.split(&object(1, 900), 512).unwrap();
        fragments[0].payload[0] ^= 1;
        let mut assembler = codec.assembler(Limits::default()).unwrap();
        assembler
            .push(fragments[0].clone(), now, now + Duration::from_millis(100))
            .unwrap();
        assert!(matches!(
            assembler.push(fragments[1].clone(), now, now + Duration::from_millis(100)),
            Err(Error::Protocol("repair integrity"))
        ));
        assert_eq!(assembler.reserved_bytes(), 0);
        assert_eq!(
            assembler
                .push(fragments[2].clone(), now, now + Duration::from_millis(100))
                .unwrap(),
            Admission::Stale
        );
    }
    #[test]
    fn counts_lengths_and_extra_bytes_fail_before_payload_copy() {
        let fragment = codec().split(&object(1, 1000), 512).unwrap().remove(0);
        for offset in [0, 16, 20, 21, 22] {
            let mut bytes = fragment.encode();
            bytes[offset] = 255;
            assert!(Fragment::decode(&bytes).is_err());
        }
        let mut trailing = fragment.encode();
        trailing.push(0);
        assert!(Fragment::decode(&trailing).is_err());
        assert!(Fragment::decode(&fragment.encode()[..55]).is_err());
        let mut short = fragment.encode();
        short.pop();
        assert!(Fragment::decode(&short).is_err());
    }
    #[test]
    fn group_and_byte_caps_preserve_existing_work_and_history_stays_bounded() {
        let codec = codec();
        let now = Instant::now();
        let parts = codec.split(&object(1, 1000), 512).unwrap();
        for limits in [
            Limits {
                groups: 1,
                ..Limits::default()
            },
            Limits {
                bytes: parts[0].header.reservation(),
                ..Limits::default()
            },
        ] {
            let mut assembler = codec.assembler(limits).unwrap();
            assembler
                .push(parts[0].clone(), now, now + Duration::from_secs(1))
                .unwrap();
            let later = codec.split(&object(2, 1000), 512).unwrap();
            assert_eq!(
                assembler
                    .push(later[0].clone(), now, now + Duration::from_secs(1))
                    .unwrap(),
                Admission::AtCapacity
            );
            assert_eq!(assembler.pending_groups(), 1);
            assert_eq!(assembler.reserved_bytes(), parts[0].header.reservation());
        }
        let mut assembler = codec.assembler(Limits::default()).unwrap();
        for id in 1..=140 {
            let fragments = codec.split(&object(id, 100), 64).unwrap();
            for fragment in fragments.iter().take(2) {
                assembler
                    .push(fragment.clone(), now, now + Duration::from_secs(1))
                    .unwrap();
            }
        }
        assert_eq!(assembler.seen.len(), HISTORY);
        assert_eq!(assembler.reserved_bytes(), 0);
        assert_eq!(
            assembler
                .push(parts[0].clone(), now, now + Duration::from_secs(1))
                .unwrap(),
            Admission::Stale
        );
    }
    #[test]
    fn redundancy_budget_charges_all_added_headers_and_padding_without_clock_minting() {
        let codec = codec();
        let now = Instant::now();
        let original = object(1, 200);
        let fragments = codec.split(&original, 128).unwrap();
        let actual = fragments
            .iter()
            .map(|f| f.encoded_len() + 25)
            .sum::<usize>()
            - original.encode_packet().unwrap().len();
        assert_eq!(actual, fragments[0].protection_overhead());
        assert_eq!(actual, 402);
        let mut budget = RepairBudget::new(500, 500, now).unwrap();
        assert!(
            codec
                .protect(&original, 128, &mut budget, now)
                .unwrap()
                .is_some()
        );
        assert!(
            codec
                .protect(&original, 128, &mut budget, now)
                .unwrap()
                .is_none()
        );
        assert!(!budget.reserve(fragments.last().unwrap(), now - Duration::from_millis(1)));
        assert!(
            codec
                .protect(
                    &original,
                    128,
                    &mut budget,
                    now + Duration::from_millis(500)
                )
                .unwrap()
                .is_none()
        );
        assert!(
            codec
                .protect(&original, 128, &mut budget, now + Duration::from_secs(1))
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn configuration_and_envelope_bounds_are_explicit() {
        let codec = codec();
        let now = Instant::now();
        assert!(codec.split(&object(1, 32744), 1024).is_err());
        assert!(codec.split(&object(0, 100), 64).is_err());
        let mut reserved_envelope = object(1, 100);
        reserved_envelope.duration_us = 0;
        assert!(codec.split(&reserved_envelope, 64).is_err());
        assert!(codec.split(&object(1, 1), 1024).is_err());
        assert!(
            codec
                .assembler(Limits {
                    groups: 9,
                    ..Limits::default()
                })
                .is_err()
        );
        assert!(
            codec
                .assembler(Limits {
                    max_age: Duration::from_millis(301),
                    ..Limits::default()
                })
                .is_err()
        );
        assert!(RepairBudget::new(0, 500, now).is_err());
        let fragment = codec.split(&object(1, 100), 64).unwrap().remove(0);
        assert!(fragment.to_object(0).is_err());
        let mut envelope = fragment.to_object(1).unwrap();
        envelope.independent = true;
        assert!(Fragment::from_object(envelope).is_err());
    }
}
