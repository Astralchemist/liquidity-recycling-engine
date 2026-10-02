use market_events::*;
#[test]
fn snapshot_overlap_contiguity_and_poison() {
    let mut t = NativeSequenceTracker::default();
    let r = |first, last, previous_last| NativeUpdateRange {
        first,
        last,
        previous_last,
    };
    assert_eq!(
        t.accept(r(1, 2, None)),
        Err(SequenceError::SnapshotRequired)
    );
    t.reset_from_snapshot(10);
    t.accept(r(8, 12, None)).unwrap();
    t.accept(r(13, 15, Some(12))).unwrap();
    assert_eq!(t.accept(r(17, 18, None)), Err(SequenceError::Gap));
    assert_eq!(t.last(), Some(15));
    assert_eq!(
        t.accept(r(16, 18, None)),
        Err(SequenceError::SnapshotRequired)
    );
    for (range, error) in [
        (r(15, 15, None), SequenceError::Duplicate),
        (r(17, 16, None), SequenceError::InvalidRange),
        (r(16, 17, Some(14)), SequenceError::PreviousMismatch),
    ] {
        t.reset_from_snapshot(15);
        assert_eq!(t.accept(range), Err(error));
    }
    t.reset_from_snapshot(u64::MAX);
    assert_eq!(
        t.accept(r(u64::MAX, u64::MAX, None)),
        Err(SequenceError::Exhausted)
    );
}
