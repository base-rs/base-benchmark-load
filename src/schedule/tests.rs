// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

fn schedule(rate: u64) -> Schedule {
    Schedule::new(NonZeroU64::new(rate).unwrap())
}

#[test]
fn offsets_have_no_cumulative_rounding_drift() {
    let schedule = schedule(240_000);
    assert_eq!(schedule.offset(0), 0);
    assert_eq!(schedule.offset(1), 4_166);
    assert_eq!(schedule.offset(64), 266_666);
    assert_eq!(schedule.offset(240_000), 1_000_000_000);
    // 200 seconds later the schedule is still exact, though each interval rounds down by two thirds of a nanosecond.
    assert_eq!(schedule.offset(48_000_000), 200_000_000_000);
    assert_eq!(schedule.offset(48_000_001), 200_000_004_166);
}

#[test]
fn offsets_saturate_rather_than_wrap() {
    assert_eq!(schedule(1).offset(u64::MAX), u64::MAX);
    assert_eq!(schedule(u64::MAX).first_at_or_after(u64::MAX), u64::MAX);
}

#[test]
fn first_at_or_after_inverts_the_rounding() {
    let schedule = schedule(240_000);
    for offset in [
        0,
        1,
        4_166,
        4_167,
        266_666,
        266_667,
        999_999_999,
        1_000_000_000,
    ] {
        let ordinal = schedule.first_at_or_after(offset);
        assert!(schedule.offset(ordinal) >= offset);
        assert!(ordinal == 0 || schedule.offset(ordinal - 1) < offset);
    }
    assert_eq!(schedule.first_at_or_after(4_166), 1);
    assert_eq!(schedule.first_at_or_after(4_167), 2);
}

#[test]
fn whole_seconds_plan_exactly_rate_requests() {
    let schedule = schedule(240_000);
    let second = 1_000_000_000;
    assert_eq!(schedule.first_at_or_after(2 * second), 480_000);
    assert_eq!(
        schedule.first_at_or_after(10 * second) - schedule.first_at_or_after(2 * second),
        1_920_000
    );
    // A third of a second at 1,000/s plans 334 requests: at 0, 1, …, 333 ms.
    assert_eq!(self::schedule(1000).first_at_or_after(second / 3), 334);
}

#[test]
fn cursors_partition_the_ordinals_in_order() {
    for (workers, connections) in [(1, 1), (1, 5), (3, 3), (3, 7), (4, 64)] {
        let total = 10 * connections as u64 + 3;
        let mut seen = Vec::new();
        for worker in 0..workers {
            let mut cursor = Cursor::new(worker, workers, connections);
            let mut previous = None;
            while cursor.ordinal() < total {
                let ordinal = cursor.ordinal();
                assert!(previous < Some(ordinal), "increasing");
                // The ordinal belongs to the connection the cursor names.
                let connection = (ordinal % connections as u64) as usize;
                assert_eq!(connection % workers, worker);
                assert_eq!(connection / workers, cursor.local());
                previous = Some(ordinal);
                seen.push(ordinal);
                cursor.advance();
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..total).collect::<Vec<_>>());
    }
}

#[test]
fn count_below_counts_a_stride() {
    assert_eq!(count_below(3, 4, 3), 0);
    assert_eq!(count_below(3, 4, 4), 1);
    assert_eq!(count_below(3, 4, 7), 1);
    assert_eq!(count_below(3, 4, 8), 2);
    assert_eq!(count_below(9, 4, 2), 0);
}
