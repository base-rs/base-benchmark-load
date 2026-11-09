// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::{clock::StepClock, statistics::InvalidKind};
use std::num::NonZeroU64;

const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: test\r\n\r\n";
const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const MS: u64 = 1_000_000;
/// The start of the test runs, so that times before it are representable.
const START: u64 = 1000 * MS;

/// A scripted stream: reads return the queued chunks, one a read, then would block; writes accept everything unless a result is queued. Each chunk stands for what arrived before one read, so a chunk that follows another arrived after the read that took it, and would give an edge-triggered socket another event.
#[derive(Debug, Default)]
struct Script {
    reads: VecDeque<io::Result<Vec<u8>>>,
    /// How many reads were made.
    read_calls: usize,
    writes: VecDeque<io::Result<usize>>,
    written: Vec<u8>,
}

impl Read for Script {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.read_calls += 1;
        match self.reads.pop_front() {
            None => Err(io::ErrorKind::WouldBlock.into()),
            Some(Err(error)) => Err(error),
            Some(Ok(mut bytes)) => {
                let length = bytes.len().min(buffer.len());
                buffer[..length].copy_from_slice(&bytes[..length]);
                if length < bytes.len() {
                    self.reads.push_front(Ok(bytes.split_off(length)));
                }
                Ok(length)
            }
        }
    }
}

impl Write for Script {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = match self.writes.pop_front() {
            None => bytes.len(),
            Some(Err(error)) => return Err(error),
            Some(Ok(length)) => length.min(bytes.len()),
        };
        self.written.extend_from_slice(&bytes[..length]);
        Ok(length)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Fixture {
    expectation: Expectation,
}

impl Fixture {
    fn new() -> Self {
        Self {
            expectation: Expectation {
                status: 200,
                body: Some(b"ok".to_vec()),
                ..Expectation::default()
            },
        }
    }

    /// An open loop at 1,000 requests per second, with a 2 ms warmup and a 10 ms window.
    fn rate(&self, connections: u64, depth: usize) -> Settings<'_> {
        Settings {
            timing: Timing {
                closed: false,
                start: START,
                window_start: START + 2 * MS,
                window_end: START + 12 * MS,
                interval: 5 * MS,
            },
            schedule: Some(Schedule::new(NonZeroU64::new(1000).unwrap())),
            total: 12,
            measured_first: 2,
            depth,
            connections,
            timeout: 100 * MS,
            connect_timeout: 100 * MS,
            reconnect_delay: MS,
            drain_deadline: START + 1000 * MS,
            request: REQUEST,
            head: false,
            expectation: &self.expectation,
        }
    }

    fn closed(&self, depth: usize) -> Settings<'_> {
        Settings {
            timing: Timing {
                closed: true,
                ..self.rate(1, depth).timing
            },
            schedule: None,
            total: 0,
            measured_first: 0,
            ..self.rate(1, depth)
        }
    }
}

fn open(settings: &Settings, script: Script, now: u64) -> (Connection<Script>, WorkerStatistics) {
    let mut statistics = WorkerStatistics::new(&settings.timing);
    let mut connection = Connection::new(0);
    connection.connecting(script, now);
    assert_eq!(connection.state, State::Connecting { since: now });
    connection.connected(now, &mut statistics);
    (connection, statistics)
}

/// A connection whose next request is ordinal 2, the first measured one.
fn open_measured(settings: &Settings, script: Script) -> (Connection<Script>, WorkerStatistics) {
    let (mut connection, statistics) = open(settings, script, START);
    connection.next_ordinal = 2;
    (connection, statistics)
}

fn script<const N: usize>(reads: [&'static [u8]; N]) -> Script {
    Script {
        reads: reads.into_iter().map(|bytes| Ok(bytes.to_vec())).collect(),
        ..Script::default()
    }
}

#[test]
fn open_loop_requests_record_their_timeline_exactly() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    let (mut connection, mut statistics) = open(&settings, script([]), START - MS);
    // Ordinal 2 is the first measured request, due at START + 2 ms; skip the warmup.
    connection.next_ordinal = 2;
    let sent = START + 2 * MS + 300;
    assert_eq!(connection.dispatch(sent, 3, &settings, &mut statistics), 1);
    // Nothing more is due, and the connection is full.
    assert_eq!(connection.dispatch(sent, 12, &settings, &mut statistics), 0);
    connection.flush(&mut statistics).unwrap();
    assert_eq!(connection.stream.as_ref().unwrap().written, REQUEST);
    assert!(!connection.pending_output());
    connection
        .stream
        .as_mut()
        .unwrap()
        .reads
        .push_back(Ok(RESPONSE.to_vec()));
    let clock = StepClock::new(sent + 1000, 0);
    connection
        .readable(&clock, false, &settings, &mut statistics)
        .unwrap();
    let distributions = &statistics.distributions;
    assert_eq!(distributions.latency.max(), Some(1300));
    assert_eq!(distributions.round_trip.max(), Some(1000));
    assert_eq!(distributions.send_delay.max(), Some(300));
    assert_eq!(distributions.dispatch_lag.max(), Some(300));
    assert_eq!(statistics.period(Period::Measured).valid, 1);
    assert_eq!(statistics.sent_in_window, 1);
    assert_eq!(statistics.completed_in_window, 1);
    assert_eq!(statistics.intervals[0].completions, 1);
    assert_eq!(statistics.bytes_read, RESPONSE.len() as u64);
    assert_eq!(statistics.bytes_written, REQUEST.len() as u64);
    assert_eq!(connection.counters.valid, 1);
    assert_eq!(connection.counters.latency_max, 1300);
    assert_eq!(connection.next_ordinal, 3);
}

#[test]
fn a_late_response_delays_the_connections_later_requests_without_moving_their_schedule() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    let (mut connection, mut statistics) = open(&settings, script([]), START - MS);
    connection.next_ordinal = 2;
    connection.dispatch(START + 2 * MS, 3, &settings, &mut statistics);
    // The response takes 5.5 ms; ordinals 3 to 7 become due meanwhile.
    connection
        .stream
        .as_mut()
        .unwrap()
        .reads
        .push_back(Ok(RESPONSE.to_vec()));
    let completed = START + 7 * MS + MS / 2;
    connection
        .readable(
            &StepClock::new(completed, 0),
            false,
            &settings,
            &mut statistics,
        )
        .unwrap();
    // The backlog is sent one at a time, as each response arrives, 200 ns after it.
    for ordinal in 3..8u64 {
        let sent = completed + (ordinal - 3) * 1000 + 200;
        assert_eq!(connection.dispatch(sent, 8, &settings, &mut statistics), 1);
        connection
            .stream
            .as_mut()
            .unwrap()
            .reads
            .push_back(Ok(RESPONSE.to_vec()));
        connection
            .readable(
                &StepClock::new(sent + 800, 0),
                false,
                &settings,
                &mut statistics,
            )
            .unwrap();
    }
    let distributions = &statistics.distributions;
    assert_eq!(distributions.latency.count(), 6);
    // Ordinal 7, planned at START + 7 ms, completed last at START + 7.5 ms + 5 µs.
    assert_eq!(distributions.latency.min(), Some(505_000));
    assert_eq!(distributions.latency.max(), Some(5_500_000));
    assert_eq!(distributions.send_delay.max(), Some(4_500_200));
    // Each backlogged request became eligible when the previous response arrived.
    assert_eq!(distributions.dispatch_lag.max(), Some(200));
    assert_eq!(distributions.round_trip.min(), Some(800));
}

#[test]
fn pipelined_requests_complete_from_one_read() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 3);
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    connection.next_ordinal = 2;
    let sent = START + 4 * MS;
    assert_eq!(connection.dispatch(sent, 12, &settings, &mut statistics), 3);
    connection.flush(&mut statistics).unwrap();
    assert_eq!(
        connection.stream.as_ref().unwrap().written,
        REQUEST.repeat(3)
    );
    // Three responses, the last split across reads: its tail arrives after the read that took the rest, and has an event of its own.
    let mut bytes = RESPONSE.repeat(3);
    let tail = bytes.split_off(bytes.len() - 5);
    let stream = connection.stream.as_mut().unwrap();
    stream.reads.push_back(Ok(bytes));
    stream.reads.push_back(Ok(tail));
    let clock = StepClock::new(sent + 100, 10);
    connection
        .readable(&clock, false, &settings, &mut statistics)
        .unwrap();
    assert_eq!(connection.in_flight.len(), 1);
    connection
        .readable(&clock, false, &settings, &mut statistics)
        .unwrap();
    assert!(connection.in_flight.is_empty());
    assert_eq!(connection.stream.as_ref().unwrap().read_calls, 2);
    assert_eq!(statistics.period(Period::Measured).valid, 3);
    // Ordinals 2, 3, and 4 were planned 2, 1, and 0 ms before they were sent together.
    assert_eq!(statistics.distributions.send_delay.max(), Some(2 * MS));
    assert_eq!(statistics.distributions.send_delay.min(), Some(0));
    // The first completion made room in a full connection; later sends are eligible from then.
    assert_eq!(connection.available, sent + 100);
}

#[test]
fn writes_continue_after_a_full_send_buffer() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 2);
    let mut script = script([]);
    script.writes = VecDeque::from([
        Ok(5),
        Err(io::ErrorKind::Interrupted.into()),
        Err(io::ErrorKind::WouldBlock.into()),
    ]);
    let (mut connection, mut statistics) = open(&settings, script, START);
    connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics);
    connection.flush(&mut statistics).unwrap();
    assert!(connection.pending_output());
    assert_eq!(statistics.write_would_block, 1);
    connection.flush(&mut statistics).unwrap();
    assert!(!connection.pending_output());
    assert_eq!(
        connection.stream.as_ref().unwrap().written,
        REQUEST.repeat(2)
    );
}

/// A read that returns less than the buffer offered found nothing more queued, so the connection waits for the next event instead of reading again only to find that the socket would block. Whatever arrives later gives the socket that event. A spurious event, for bytes an earlier read already took, finds nothing to read.
#[test]
fn a_short_read_waits_for_the_next_event() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 2);
    let (mut connection, mut statistics) = open_measured(&settings, script([RESPONSE, RESPONSE]));
    connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics);
    let clock = StepClock::new(START + 4 * MS, 0);
    let reads = |connection: &Connection<Script>| connection.stream.as_ref().unwrap().read_calls;
    for (event, valid) in [(1, 1), (2, 2), (3, 2)] {
        connection
            .readable(&clock, false, &settings, &mut statistics)
            .unwrap();
        assert_eq!(reads(&connection), event);
        assert_eq!(statistics.period(Period::Measured).valid, valid);
    }
    // Reported with the close, the same response is followed by the end of the stream, which no later event would report.
    let (mut connection, mut statistics) = open_measured(&settings, script([RESPONSE, b""]));
    connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics);
    assert_eq!(
        connection.readable(&clock, true, &settings, &mut statistics),
        Err(ErrorKind::Eof)
    );
    assert_eq!(reads(&connection), 2);
    assert_eq!(statistics.period(Period::Measured).valid, 1);
}

#[test]
fn write_failures_are_classified() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    for (result, kind) in [
        (Ok(0), ErrorKind::Write),
        (Err(io::ErrorKind::ConnectionReset.into()), ErrorKind::Reset),
        (Err(io::ErrorKind::BrokenPipe.into()), ErrorKind::Reset),
        (
            Err(io::ErrorKind::ConnectionAborted.into()),
            ErrorKind::Reset,
        ),
        (
            Err(io::ErrorKind::PermissionDenied.into()),
            ErrorKind::Write,
        ),
    ] {
        let mut script = script([]);
        script.writes.push_back(result);
        let (mut connection, mut statistics) = open(&settings, script, START);
        connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics);
        assert_eq!(connection.flush(&mut statistics), Err(kind));
    }
}

#[test]
fn read_failures_are_classified() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    for (error, kind) in [
        (io::ErrorKind::ConnectionReset, ErrorKind::Reset),
        (io::ErrorKind::TimedOut, ErrorKind::Read),
    ] {
        let mut script = script([]);
        script.reads = VecDeque::from([Err(io::ErrorKind::Interrupted.into()), Err(error.into())]);
        let (mut connection, mut statistics) = open(&settings, script, START);
        assert_eq!(
            connection.readable(&StepClock::new(START, 1), false, &settings, &mut statistics),
            Err(kind)
        );
    }
}

#[test]
fn failures_are_charged_to_every_outstanding_request() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 4);
    // A response, then the end of the stream with three requests outstanding, reported by one event.
    let (mut connection, mut statistics) = open(&settings, script([RESPONSE, b""]), START);
    connection.next_ordinal = 1;
    connection.dispatch(START + 5 * MS, 12, &settings, &mut statistics);
    let now = START + 6 * MS;
    let kind = connection
        .readable(&StepClock::new(now, 0), true, &settings, &mut statistics)
        .unwrap_err();
    assert_eq!(kind, ErrorKind::Eof);
    assert_eq!(statistics.idle_closes, 0);
    connection.fail(kind, now, &settings, &mut statistics);
    // Ordinal 1 was warmup; 2, 3, and 4 were measured.
    assert_eq!(statistics.period(Period::Warmup).valid, 1);
    assert_eq!(
        statistics.period(Period::Measured).errors[ErrorKind::Eof as usize],
        3
    );
    assert_eq!(connection.counters.errors, 3);
    assert_eq!(connection.state, State::Down { until: now + MS });
    assert!(connection.stream.is_none());
    assert_eq!(statistics.disconnects, 1);
    // A down connection sends nothing.
    assert_eq!(connection.dispatch(now, 12, &settings, &mut statistics), 0);
}

#[test]
fn a_server_closing_an_idle_connection_is_not_a_request_error() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    let (mut connection, mut statistics) = open(&settings, script([&b""[..]]), START);
    assert_eq!(
        connection.readable(&StepClock::new(START, 0), false, &settings, &mut statistics),
        Err(ErrorKind::Eof)
    );
    connection.fail(ErrorKind::Eof, START, &settings, &mut statistics);
    assert_eq!(statistics.idle_closes, 1);
    assert_eq!(statistics.finished(), 0);
}

#[test]
fn unsolicited_and_malformed_bytes_are_parse_errors() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    let (mut connection, mut statistics) = open(&settings, script([RESPONSE]), START);
    assert_eq!(
        connection.readable(&StepClock::new(START, 0), false, &settings, &mut statistics),
        Err(ErrorKind::Parse)
    );
    let (mut connection, mut statistics) =
        open(&settings, script([&b"HTTP/1.1 2000\r\n\r\n"[..]]), START);
    connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics);
    assert_eq!(
        connection.readable(&StepClock::new(START, 0), false, &settings, &mut statistics),
        Err(ErrorKind::Parse)
    );
}

#[test]
fn a_closing_response_ends_the_connection_after_it_completes() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 2);
    let closing: &[u8] = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok";
    let (mut connection, mut statistics) = open_measured(&settings, script([closing]));
    connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics);
    let now = START + 4 * MS;
    let kind = connection
        .readable(&StepClock::new(now, 0), false, &settings, &mut statistics)
        .unwrap_err();
    assert_eq!(kind, ErrorKind::Closed);
    connection.fail(kind, now, &settings, &mut statistics);
    let measured = statistics.period(Period::Measured);
    assert_eq!(
        (measured.valid, measured.errors[ErrorKind::Closed as usize]),
        (1, 1)
    );
}

#[test]
fn close_delimited_responses_complete_at_the_end_of_the_stream() {
    let fixture = Fixture::new();
    // At depth 2, a second request is pipelined behind the close-delimited response, which ends it as a closing response would.
    let settings = fixture.rate(1, 2);
    let (mut connection, mut statistics) = open_measured(
        &settings,
        script([&b"HTTP/1.1 200 OK\r\n\r\no"[..], b"k", b""]),
    );
    assert_eq!(
        connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics),
        2
    );
    // The body's last byte and the close arrive together, after the head and the first byte; their event reports the close.
    let now = START + 4 * MS;
    let clock = StepClock::new(now, 0);
    connection
        .readable(&clock, false, &settings, &mut statistics)
        .unwrap();
    let kind = connection
        .readable(&clock, true, &settings, &mut statistics)
        .unwrap_err();
    assert_eq!(kind, ErrorKind::Closed);
    connection.fail(kind, now, &settings, &mut statistics);
    let measured = statistics.period(Period::Measured);
    assert_eq!(
        (
            measured.valid,
            measured.errors[ErrorKind::Closed as usize],
            measured.errors[ErrorKind::Eof as usize]
        ),
        (1, 1, 0)
    );
    assert_eq!(statistics.idle_closes, 0);
}

#[test]
fn invalid_responses_are_counted_apart_from_latency() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    let (mut connection, mut statistics) = open_measured(
        &settings,
        script([&b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nno"[..]]),
    );
    connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics);
    connection
        .readable(
            &StepClock::new(START + 4 * MS, 0),
            false,
            &settings,
            &mut statistics,
        )
        .unwrap();
    let measured = statistics.period(Period::Measured);
    assert_eq!(
        (measured.valid, measured.invalid[InvalidKind::Body as usize]),
        (0, 1)
    );
    assert_eq!(statistics.distributions.latency.count(), 0);
    assert_eq!(connection.counters.invalid, 1);
}

#[test]
fn large_heads_grow_the_input_buffer() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 2);
    let mut bytes = RESPONSE.to_vec();
    bytes.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX: ");
    bytes.resize(40 * 1024, b'x');
    bytes.extend_from_slice(b"\r\n\r\nok");
    let mut script = Script::default();
    script.reads.push_back(Ok(bytes));
    let (mut connection, mut statistics) = open_measured(&settings, script);
    connection.dispatch(START + 3 * MS, 12, &settings, &mut statistics);
    // The first read fills the buffer; the first response is consumed, then the rest of the head moves to the front and the buffer grows until the head fits.
    connection
        .readable(
            &StepClock::new(START + 4 * MS, 0),
            false,
            &settings,
            &mut statistics,
        )
        .unwrap();
    assert_eq!(statistics.period(Period::Measured).valid, 2);
    assert!(connection.input.len() >= 40 * 1024);
}

#[test]
fn timeouts_count_from_the_send() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    assert!(!connection.timed_out(u64::MAX, &settings));
    let sent = START + 3 * MS;
    connection.dispatch(sent, 12, &settings, &mut statistics);
    assert!(!connection.timed_out(sent + 100 * MS - 1, &settings));
    assert!(connection.timed_out(sent + 100 * MS, &settings));
}

#[test]
fn abandoning_counts_unsent_requests_by_period() {
    let fixture = Fixture::new();
    // Two connections: this one sends the even ordinals 0 to 10.
    let settings = fixture.rate(2, 1);
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    connection.dispatch(START, 1, &settings, &mut statistics);
    connection.abandon(START + 1000 * MS, &settings, &mut statistics);
    let (warmup, measured) = (
        statistics.period(Period::Warmup),
        statistics.period(Period::Measured),
    );
    // Ordinal 0 timed out; ordinal 2 onwards, all measured, were never sent.
    assert_eq!(warmup.errors[ErrorKind::Timeout as usize], 1);
    assert_eq!(measured.errors[ErrorKind::Unsent as usize], 5);
    assert_eq!(statistics.finished(), 6);
    assert_eq!(connection.state, State::Down { until: u64::MAX });
    assert!(!connection.has_work(0, &settings));
    // Ordinal 0, planned at the start, waited a second unanswered; ordinal 2, the oldest never sent, waited 2 ms less.
    assert_eq!(
        (
            connection.counters.wait_max,
            connection.counters.wait_unanswered
        ),
        (1000 * MS, true)
    );
    // Abandoned again, with every request accounted for, nothing more waited.
    connection.counters.wait_max = 0;
    connection.abandon(START + 2000 * MS, &settings, &mut statistics);
    assert_eq!(connection.counters.wait_max, 0);
    assert_eq!(statistics.finished(), 6);
    // A connection that never connected still accounts for its warmup.
    let mut connection = Connection::<Script>::new(1);
    let mut statistics = WorkerStatistics::new(&settings.timing);
    connection.abandon(START, &settings, &mut statistics);
    assert_eq!(
        statistics.period(Period::Warmup).errors[ErrorKind::Unsent as usize],
        1
    );
    assert_eq!(
        statistics.period(Period::Measured).errors[ErrorKind::Unsent as usize],
        5
    );
    assert_eq!(statistics.disconnects, 0);
    // Abandoned before the window, its requests were never outstanding in it, so none waited in it.
    assert_eq!(connection.counters.wait_max, 0);
    // Abandoned at the window's end, the oldest never sent, ordinal 1, waited from its planned time.
    let mut connection = Connection::<Script>::new(1);
    connection.abandon(START + 12 * MS, &settings, &mut statistics);
    assert_eq!(
        (
            connection.counters.wait_max,
            connection.counters.wait_unanswered
        ),
        (11 * MS, true)
    );
}

#[test]
fn closed_loop_requests_continue_until_the_window_ends() {
    let fixture = Fixture::new();
    let settings = fixture.closed(2);
    let (mut connection, mut statistics) = open(&settings, script([]), START - MS);
    assert!(connection.has_work(START, &settings));
    assert_eq!(connection.dispatch(START, 0, &settings, &mut statistics), 2);
    connection
        .stream
        .as_mut()
        .unwrap()
        .reads
        .push_back(Ok(RESPONSE.repeat(2)));
    // Completed in the warmup.
    connection
        .readable(
            &StepClock::new(START + MS, 0),
            false,
            &settings,
            &mut statistics,
        )
        .unwrap();
    assert_eq!(
        connection.dispatch(START + MS + 10, 0, &settings, &mut statistics),
        2
    );
    connection
        .stream
        .as_mut()
        .unwrap()
        .reads
        .push_back(Ok(RESPONSE.to_vec()));
    // One in the window, by its completion.
    connection
        .readable(
            &StepClock::new(START + 3 * MS, 0),
            false,
            &settings,
            &mut statistics,
        )
        .unwrap();
    assert_eq!(statistics.distributions.round_trip.max(), Some(2 * MS - 10));
    assert_eq!(statistics.distributions.dispatch_lag.max(), Some(10));
    assert_eq!(statistics.distributions.latency.count(), 0);
    assert_eq!(statistics.intervals[0].latency.count(), 1);
    // After the window, nothing more is sent, and the last completion is drain.
    let end = START + 12 * MS;
    assert!(!connection.has_work(end, &settings));
    assert_eq!(connection.dispatch(end, 0, &settings, &mut statistics), 0);
    connection
        .stream
        .as_mut()
        .unwrap()
        .reads
        .push_back(Ok(RESPONSE.to_vec()));
    connection
        .readable(&StepClock::new(end, 0), false, &settings, &mut statistics)
        .unwrap();
    let periods = [Period::Warmup, Period::Measured, Period::Drain]
        .map(|period| statistics.period(period).valid);
    assert_eq!(periods, [2, 1, 1]);
}

#[test]
fn connection_failures_allow_a_retry_after_the_delay() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    let mut statistics = WorkerStatistics::new(&settings.timing);
    let mut connection = Connection::new(0);
    assert!(connection.has_work(0, &settings));
    connection.connecting(Script::default(), START);
    connection.connect_failed(START + 5, &settings, &mut statistics);
    assert_eq!(
        connection.state,
        State::Down {
            until: START + 5 + MS
        }
    );
    assert!(connection.stream.is_none());
    assert_eq!(statistics.connect_errors, 1);
}

/// Deliver one valid response at `now`.
fn respond(
    connection: &mut Connection<Script>,
    now: u64,
    settings: &Settings,
    statistics: &mut WorkerStatistics,
) {
    connection
        .stream
        .as_mut()
        .unwrap()
        .reads
        .push_back(Ok(RESPONSE.to_vec()));
    connection
        .readable(&StepClock::new(now, 0), false, settings, statistics)
        .unwrap();
}

/// The longest starved span, and its part in the window.
fn spans(connection: &Connection<Script>) -> (u64, u64) {
    (
        connection.counters.span_max,
        connection.counters.span_window_max,
    )
}

/// In the open loop, a slot's starved span begins at the planned time of the request it waits for, ends at each response, and continues from it while a request is due, across failures and reconnections, until the drain deadline abandons what is left.
#[test]
fn an_open_loop_starved_span_runs_across_failures_and_reconnections() {
    let fixture = Fixture::new();
    let settings = fixture.rate(1, 1);
    let (mut connection, mut statistics) = open_measured(&settings, script([]));
    // Ordinal 2, planned at 2 ms, is sent 300 ns late; the span begins at its planned time.
    connection.dispatch(START + 2 * MS + 300, 3, &settings, &mut statistics);
    assert_eq!(connection.counters.span_since, Some(START + 2 * MS));
    // Its response at 7.5 ms ends the span, and ordinals 3 to 7 are due, so another begins.
    respond(
        &mut connection,
        START + 7 * MS + MS / 2,
        &settings,
        &mut statistics,
    );
    assert_eq!(spans(&connection), (5 * MS + MS / 2, 5 * MS + MS / 2));
    assert_eq!(
        connection.counters.span_since,
        Some(START + 7 * MS + MS / 2)
    );
    connection.dispatch(START + 8 * MS, 8, &settings, &mut statistics);
    // A reset fails ordinal 3, but ordinal 4 is due, so the span stays open while the slot reconnects.
    connection.fail(ErrorKind::Reset, START + 9 * MS, &settings, &mut statistics);
    assert_eq!(
        connection.counters.span_since,
        Some(START + 7 * MS + MS / 2)
    );
    let (mut reconnected, _) = open(&settings, script([]), START + 10 * MS);
    connection.stream = reconnected.stream.take();
    connection.connected(START + 10 * MS, &mut statistics);
    connection.dispatch(START + 10 * MS, 12, &settings, &mut statistics);
    // Its request times out after the window, with later ones due: still open.
    connection.fail(
        ErrorKind::Timeout,
        START + 15 * MS,
        &settings,
        &mut statistics,
    );
    assert_eq!(
        connection.counters.span_since,
        Some(START + 7 * MS + MS / 2)
    );
    // The drain deadline ends it, 12.5 ms long, 4.5 ms of it in the window.
    connection.abandon(START + 20 * MS, &settings, &mut statistics);
    assert_eq!(connection.counters.span_since, None);
    assert_eq!(spans(&connection), (12 * MS + MS / 2, 5 * MS + MS / 2));
    assert_eq!(connection.counters.valid, 1);
}

/// In the open loop, a slot with nothing outstanding or due has no starved span: a response or a failure with nothing due ends it, and the next request's planned time begins another. A span counts only from the window's start.
#[test]
fn an_open_loop_slot_with_nothing_due_is_not_starved() {
    let fixture = Fixture::new();
    // Four connections: this one has ordinals 0, 4, and 8.
    let settings = fixture.rate(4, 1);
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    connection.dispatch(START, 1, &settings, &mut statistics);
    // Answered in the warmup, with nothing due: the span ends, before the window, so it counts for nothing.
    respond(&mut connection, START + MS, &settings, &mut statistics);
    assert_eq!(connection.counters.span_since, None);
    assert_eq!(spans(&connection), (0, 0));
    // Ordinal 4 is planned at 4 ms, sent late, and answered at 5 ms, with ordinal 8 not due until 8 ms.
    connection.dispatch(START + 4 * MS + 500, 5, &settings, &mut statistics);
    assert_eq!(connection.counters.span_since, Some(START + 4 * MS));
    respond(&mut connection, START + 5 * MS, &settings, &mut statistics);
    assert_eq!(spans(&connection), (MS, MS));
    assert_eq!(connection.counters.span_since, None);
    // Ordinal 8 fails, the slot's last: nothing is due, so its span ends with the failure.
    connection.dispatch(START + 8 * MS, 9, &settings, &mut statistics);
    connection.fail(
        ErrorKind::Reset,
        START + 10 * MS,
        &settings,
        &mut statistics,
    );
    assert_eq!(connection.counters.span_since, None);
    assert_eq!(spans(&connection), (2 * MS, 2 * MS));
    // A slot that never sent a request planned in the window waits from its planned time until the drain deadline.
    let mut connection = Connection::<Script>::new(1);
    connection.next_ordinal = 5;
    connection.abandon(START + 30 * MS, &settings, &mut statistics);
    assert_eq!(spans(&connection), (25 * MS, 7 * MS));
}

/// In the closed loop, a slot always has a request to send in the window, so its starved span runs from its last response across failures, until the window's end if it has nothing in flight then, or until its requests in flight have outcomes in the drain.
#[test]
fn a_closed_loop_starved_span_ends_with_the_slots_last_outcome() {
    let fixture = Fixture::new();
    let settings = fixture.closed(1);
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    // Sent in the warmup and answered in the window: only the part from the window's start counts.
    connection.dispatch(START + MS, 0, &settings, &mut statistics);
    respond(&mut connection, START + 3 * MS, &settings, &mut statistics);
    assert_eq!(spans(&connection), (MS, MS));
    assert_eq!(connection.counters.span_since, Some(START + 3 * MS));
    connection.dispatch(START + 3 * MS, 0, &settings, &mut statistics);
    // A reset in the window leaves the slot with a request to send, so the span stays open, and with nothing in flight, it ends at the window's end.
    connection.fail(ErrorKind::Reset, START + 4 * MS, &settings, &mut statistics);
    assert_eq!(connection.counters.span_since, Some(START + 3 * MS));
    connection.finish(START + 20 * MS, &settings);
    assert_eq!(spans(&connection), (9 * MS, 9 * MS));
    // Connecting at the drain deadline, with nothing in flight, the slot stopped waiting at the window's end.
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    connection.dispatch(START + 5 * MS, 0, &settings, &mut statistics);
    connection.fail(ErrorKind::Reset, START + 6 * MS, &settings, &mut statistics);
    let (mut reconnecting, _) = open(&settings, script([]), START + 7 * MS);
    connection.connecting(reconnecting.stream.take().unwrap(), START + 7 * MS);
    connection.abandon(START + 30 * MS, &settings, &mut statistics);
    assert_eq!(spans(&connection), (7 * MS, 7 * MS));
    // A request in flight at the window's end keeps the span open into the drain, until its outcome.
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    connection.dispatch(START + 11 * MS, 0, &settings, &mut statistics);
    respond(&mut connection, START + 15 * MS, &settings, &mut statistics);
    assert_eq!(spans(&connection), (4 * MS, MS));
    assert_eq!(connection.counters.span_since, None);
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    connection.dispatch(START + 11 * MS, 0, &settings, &mut statistics);
    connection.fail(
        ErrorKind::Timeout,
        START + 25 * MS,
        &settings,
        &mut statistics,
    );
    assert_eq!(spans(&connection), (14 * MS, MS));
    // Pipelined: a response with another request still in flight ends one span and begins the next.
    let settings = fixture.closed(2);
    let (mut connection, mut statistics) = open(&settings, script([]), START);
    assert_eq!(
        connection.dispatch(START + 11 * MS, 0, &settings, &mut statistics),
        2
    );
    respond(&mut connection, START + 13 * MS, &settings, &mut statistics);
    assert_eq!(connection.counters.span_since, Some(START + 13 * MS));
    respond(&mut connection, START + 16 * MS, &settings, &mut statistics);
    assert_eq!(spans(&connection), (3 * MS, MS));
    assert_eq!(connection.counters.span_since, None);
}
