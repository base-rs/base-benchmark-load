// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

const MS: u64 = 1_000_000;

fn timing(closed: bool) -> Timing {
    Timing {
        closed,
        start: 100 * MS,
        window_start: 110 * MS,
        window_end: 120 * MS,
        interval: 3 * MS,
    }
}

fn request(planned: u64, sent: u64) -> Request {
    Request {
        ordinal: 0,
        planned,
        sent,
        eligible: planned,
    }
}

#[test]
fn names_are_stable() {
    let names: Vec<_> = ErrorKind::ALL.iter().map(|kind| kind.name()).collect();
    assert_eq!(
        names,
        [
            "write", "read", "reset", "eof", "timeout", "parse", "closed", "unsent"
        ]
    );
    let names: Vec<_> = InvalidKind::ALL.iter().map(|kind| kind.name()).collect();
    assert_eq!(names, ["status", "header", "body", "response"]);
    let names: Vec<_> = Period::ALL.iter().map(|period| period.name()).collect();
    assert_eq!(names, ["warmup", "measured", "drain"]);
}

#[test]
fn open_loop_outcomes_belong_to_their_planned_period() {
    let timing = timing(false);
    assert_eq!(timing.interval_count(), 4);
    let mut statistics = WorkerStatistics::new(&timing);
    let mut connection = ConnectionCounters::default();
    // Planned in the warmup, completed in the window: warmup.
    statistics.complete(
        &timing,
        &request(109 * MS, 109 * MS),
        111 * MS,
        None,
        &mut connection,
    );
    // Planned in the window, completed long after it: measured.
    statistics.complete(
        &timing,
        &request(119 * MS, 119 * MS),
        150 * MS,
        None,
        &mut connection,
    );
    statistics.fail(
        &timing,
        &request(112 * MS, 113 * MS),
        200 * MS,
        ErrorKind::Timeout,
        &mut connection,
    );
    statistics.complete(
        &timing,
        &request(110 * MS, 110 * MS),
        111 * MS,
        Some(InvalidKind::Body),
        &mut connection,
    );
    statistics.complete(
        &timing,
        &request(105 * MS, 105 * MS),
        106 * MS,
        Some(InvalidKind::Status),
        &mut connection,
    );
    statistics.fail(
        &timing,
        &request(105 * MS, 105 * MS),
        106 * MS,
        ErrorKind::Reset,
        &mut connection,
    );
    statistics.unsent(Period::Measured, 3);
    let (warmup, measured) = (
        statistics.period(Period::Warmup),
        statistics.period(Period::Measured),
    );
    assert_eq!(warmup.valid, 1);
    assert_eq!(warmup.invalid[InvalidKind::Status as usize], 1);
    assert_eq!(warmup.errors[ErrorKind::Reset as usize], 1);
    assert_eq!(measured.valid, 1);
    assert_eq!(measured.invalid_total(), 1);
    assert_eq!(measured.errors[ErrorKind::Timeout as usize], 1);
    assert_eq!(measured.errors[ErrorKind::Unsent as usize], 3);
    assert_eq!(measured.total(), 6);
    assert_eq!(statistics.finished(), 9);
    // Only the measured valid response is in the distributions, and the interval of its planned time.
    assert_eq!(statistics.distributions.latency.max(), Some(31 * MS));
    assert_eq!(statistics.intervals[3].latency.count(), 1);
    // Completions in the window count by completion time.
    assert_eq!(statistics.completed_in_window, 2);
    assert_eq!(statistics.intervals[0].completions, 2);
    assert_eq!(statistics.last_outcome, Some(200 * MS));
    assert_eq!(
        (connection.valid, connection.invalid, connection.errors),
        (1, 1, 1)
    );
}

#[test]
fn closed_loop_outcomes_belong_to_the_period_they_happen_in() {
    let timing = timing(true);
    let mut statistics = WorkerStatistics::new(&timing);
    let mut connection = ConnectionCounters::default();
    for (sent, now) in [(105, 109), (109, 110), (119, 121)] {
        statistics.complete(
            &timing,
            &request(100 * MS, sent * MS),
            now * MS,
            None,
            &mut connection,
        );
    }
    statistics.fail(
        &timing,
        &request(100 * MS, 119 * MS),
        125 * MS,
        ErrorKind::Eof,
        &mut connection,
    );
    let valid = Period::ALL.map(|period| statistics.period(period).valid);
    assert_eq!(valid, [1, 1, 1]);
    assert_eq!(
        statistics.period(Period::Drain).errors[ErrorKind::Eof as usize],
        1
    );
    // The service time, from the send; no open-loop latency is recorded.
    assert_eq!(statistics.distributions.round_trip.max(), Some(MS));
    assert_eq!(statistics.distributions.latency.count(), 0);
    assert_eq!(statistics.intervals[0].latency.max(), Some(MS));
    assert_eq!(connection.errors, 0);
}

#[test]
fn sends_in_the_window_are_counted() {
    let timing = timing(false);
    let mut statistics = WorkerStatistics::new(&timing);
    for now in [109, 110, 119, 120] {
        statistics.sent(&timing, now * MS);
    }
    assert_eq!((statistics.sent, statistics.sent_in_window), (4, 2));
}

/// A request's schedule lag is measured when it was planned in the window, and its reach is counted when it happened in the window, so that a request planned in the warmup but reached in the window counts as reached in it.
#[test]
fn reaches_are_measured_by_plan_and_counted_by_time() {
    let timing = timing(false);
    let mut statistics = WorkerStatistics::new(&timing);
    for (planned, now) in [(108, 111), (109, 112), (112, 113), (119, 121), (120, 121)] {
        statistics.reached(&timing, planned * MS, now * MS);
    }
    assert_eq!(statistics.reached_in_window, 3);
    let lag = &statistics.distributions.schedule_lag;
    assert_eq!(
        (lag.count(), lag.min(), lag.max()),
        (2, Some(MS), Some(2 * MS))
    );
}

#[test]
fn worker_statistics_merge_exactly() {
    let timing = timing(false);
    let mut connection = ConnectionCounters::default();
    let mut first = WorkerStatistics::new(&timing);
    first.complete(
        &timing,
        &request(111 * MS, 111 * MS),
        112 * MS,
        None,
        &mut connection,
    );
    first.sent(&timing, 111 * MS);
    let mut second = WorkerStatistics::new(&timing);
    second.complete(
        &timing,
        &request(113 * MS, 114 * MS),
        117 * MS,
        None,
        &mut connection,
    );
    second.fail(
        &timing,
        &request(105 * MS, 105 * MS),
        106 * MS,
        ErrorKind::Parse,
        &mut connection,
    );
    second.complete(
        &timing,
        &request(105 * MS, 105 * MS),
        106 * MS,
        Some(InvalidKind::Header),
        &mut connection,
    );
    second.reached_in_window = 11;
    second.connects = 2;
    second.connect_errors = 1;
    second.disconnects = 3;
    second.idle_closes = 4;
    second.write_would_block = 5;
    second.bytes_written = 6;
    second.bytes_read = 7;
    second.iterations = 8;
    second.busy = 9;
    second.cpu = 10;
    let mut merged = first.clone();
    merged.merge(&second);
    assert_eq!(merged.period(Period::Measured).valid, 2);
    assert_eq!(
        merged.period(Period::Warmup).errors[ErrorKind::Parse as usize],
        1
    );
    assert_eq!(
        merged.period(Period::Warmup).invalid[InvalidKind::Header as usize],
        1
    );
    assert_eq!(merged.distributions.latency.count(), 2);
    assert_eq!(merged.distributions.latency.max(), Some(4 * MS));
    assert_eq!(
        merged.intervals[0].completions + merged.intervals[2].completions,
        2
    );
    assert_eq!(merged.intervals[1].latency.count(), 1);
    assert_eq!(
        (
            merged.sent,
            merged.sent_in_window,
            merged.reached_in_window,
            merged.completed_in_window
        ),
        (1, 1, 11, 2)
    );
    assert_eq!(merged.last_outcome, Some(117 * MS));
    assert_eq!(
        (
            merged.connects,
            merged.connect_errors,
            merged.disconnects,
            merged.idle_closes,
            merged.write_would_block
        ),
        (2, 1, 3, 4, 5)
    );
    assert_eq!(
        (
            merged.bytes_written,
            merged.bytes_read,
            merged.iterations,
            merged.busy,
            merged.cpu
        ),
        (6, 7, 8, 9, 10)
    );
}
