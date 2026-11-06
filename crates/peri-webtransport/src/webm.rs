//! Streaming EBML for the compatibility player and shared transcoder. / 兼容播放器与共享转码器的流式 WebM 封装。
use peri_native::media::{Catalog, Frame, FrameKind, TrackKind};
fn elem(out: &mut Vec<u8>, id: u32, data: &[u8]) {
    let bytes = id.to_be_bytes();
    out.extend_from_slice(&bytes[(id.leading_zeros() / 8) as usize..]);
    let mut width = 1;
    while data.len() as u64 >= (1u64 << (width * 7)) - 1 {
        width += 1;
    }
    let size = (data.len() as u64 | (1u64 << (width * 7))).to_be_bytes();
    out.extend_from_slice(&size[8 - width..]);
    out.extend_from_slice(data);
}
fn uint(out: &mut Vec<u8>, id: u32, n: u64) {
    let b = n.to_be_bytes();
    elem(out, id, &b[((n.leading_zeros() / 8) as usize).min(7)..]);
}
pub fn header(c: &Catalog) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ebml = Vec::new();
    uint(&mut ebml, 0x4286, 1);
    uint(&mut ebml, 0x42f7, 1);
    uint(&mut ebml, 0x42f2, 4);
    uint(&mut ebml, 0x42f3, 8);
    elem(&mut ebml, 0x4282, b"webm");
    uint(&mut ebml, 0x4287, 4);
    uint(&mut ebml, 0x4285, 2);
    elem(&mut out, 0x1a45dfa3, &ebml);
    out.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0xff]);
    let mut info = Vec::new();
    uint(&mut info, 0x2ad7b1, 1_000_000);
    elem(&mut info, 0x4d80, b"PERI");
    elem(&mut info, 0x5741, b"PERI");
    elem(&mut out, 0x1549a966, &info);
    let mut tracks = Vec::new();
    for t in &c.tracks {
        let mut entry = Vec::new();
        let mut format = Vec::new();
        uint(&mut entry, 0xd7, t.id.into());
        uint(&mut entry, 0x73c5, t.id.into());
        match t.kind {
            TrackKind::Video => {
                uint(&mut entry, 0x83, 1);
                elem(&mut entry, 0x86, b"V_AV1");
                uint(&mut format, 0xb0, t.width.unwrap().into());
                uint(&mut format, 0xba, t.height.unwrap().into());
                elem(&mut entry, 0xe0, &format);
            }
            TrackKind::Audio => {
                uint(&mut entry, 0x83, 2);
                elem(&mut entry, 0x86, b"A_OPUS");
                elem(
                    &mut format,
                    0xb5,
                    &(t.sample_rate.unwrap() as f64).to_be_bytes(),
                );
                uint(&mut format, 0x9f, t.channels.unwrap().into());
                elem(&mut entry, 0xe1, &format);
                uint(&mut entry, 0x56bb, 80_000_000);
            }
        }
        if !t.init.is_empty() {
            elem(&mut entry, 0x63a2, &t.init);
        }
        elem(&mut tracks, 0xae, &entry);
    }
    elem(&mut out, 0x1654ae6b, &tracks);
    out
}
pub fn cluster(f: &Frame, origin: u64) -> Vec<u8> {
    let mut cluster = Vec::new();
    uint(
        &mut cluster,
        0xe7,
        f.timestamp_us.saturating_sub(origin) / 1000,
    );
    let mut block = Vec::with_capacity(f.payload.len() + 4);
    block.extend_from_slice(&[
        0x80 | f.track as u8,
        0,
        0,
        if f.kind == FrameKind::Reference {
            0
        } else {
            0x80
        },
    ]);
    block.extend_from_slice(&f.payload);
    elem(&mut cluster, 0xa3, &block);
    let mut out = Vec::new();
    elem(&mut out, 0x1f43b675, &cluster);
    out
}
