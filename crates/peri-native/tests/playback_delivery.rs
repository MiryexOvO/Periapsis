use peri_native::{
    media::{Frame, FrameKind},
    playback::{Delivery, ReorderWindow},
};
use std::time::{Duration, Instant};
fn reference(sequence: u64) -> Frame {
    Frame {
        track: 1,
        sequence,
        timestamp_us: sequence * 66667,
        duration_us: 66667,
        kind: FrameKind::Reference,
        dependencies: vec![sequence - 1],
        payload: vec![42],
    }
}
fn delivered(result: Option<Delivery>) -> (Frame, Instant) {
    match result {
        Some(Delivery::Frame(frame, arrived)) => (frame, arrived),
        other => panic!("expected frame, got {other:?}"),
    }
}

// 前驱补到后按序交付，保留首次入窗时间。
// Resolve a reversed dependency chain while preserving each original arrival.
#[test]
fn predecessor_unlocks_reversed_chain() {
    let now = Instant::now();
    let later = now + Duration::from_millis(5);
    let mut window = ReorderWindow::new(1);
    assert!(window.offer(reference(13), 10, now).is_none());
    assert!(window.offer(reference(12), 10, later).is_none());
    assert!(window.poll(10, later).is_none());
    assert_eq!(
        delivered(window.offer(reference(11), 10, later)).0.sequence,
        11
    );
    assert_eq!(delivered(window.poll(11, later)), (reference(12), later));
    assert_eq!(delivered(window.poll(12, later)), (reference(13), now));
}
// 重复片不续期；在40ms边界释放缺少前驱的帧供恢复判断。
// Duplicates cannot extend expiry. At 40 ms the caller can act on the unresolved gap.
#[test]
fn duplicate_cannot_extend_deadline() {
    let now = Instant::now();
    let due = now + Duration::from_millis(40);
    let mut window = ReorderWindow::new(1);
    assert!(window.offer(reference(13), 10, now).is_none());
    assert!(matches!(
        window.offer(reference(13), 10, due),
        Some(Delivery::Discarded)
    ));
    assert_eq!(window.deadline(), Some(due));
    assert!(window.poll(10, due - Duration::from_nanos(1)).is_none());
    assert_eq!(delivered(window.poll(10, due)), (reference(13), now));
}
// 无论调用者是否先poll，容量和恢复顺序都由窗口维护。
// Capacity and recovery order hold even when the caller offers again before polling.
#[test]
fn fifth_offer_releases_lowest_and_keeps_four() {
    for (next, first) in [(12, 12), (18, 13)] {
        let now = Instant::now();
        let mut window = ReorderWindow::new(1);
        for seq in [16, 14, 13, 15] {
            assert!(window.offer(reference(seq), 10, now).is_none());
        }
        assert_eq!(
            delivered(window.offer(reference(next), 10, now)).0.sequence,
            first
        );
        assert_eq!(
            delivered(window.poll(10, now)).0.sequence,
            if next == 12 { 13 } else { 14 }
        );
        assert_eq!(window.clear(), 3);
        assert_eq!(window.deadline(), None);
    }
}
// 切到新关键帧后丢弃旧引用，其他轨道使用自己的序号进度。
// After advancing to a keyframe, discard obsolete references and preserve unrelated tracks.
#[test]
fn late_key_and_other_track_do_not_reopen_chain() {
    let now = Instant::now();
    let mut window = ReorderWindow::new(1);
    assert!(window.offer(reference(12), 10, now).is_none());
    let key = Frame {
        kind: FrameKind::Key,
        dependencies: vec![],
        ..reference(20)
    };
    assert_eq!(delivered(window.offer(key.clone(), 10, now)).0, key);
    assert!(matches!(window.poll(20, now), Some(Delivery::Discarded)));
    assert!(matches!(
        window.offer(key, 20, now),
        Some(Delivery::Discarded)
    ));
    let other = Frame {
        track: 2,
        ..reference(3)
    };
    assert_eq!(delivered(window.offer(other.clone(), 20, now)).0, other);
}
