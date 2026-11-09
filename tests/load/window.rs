// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The measured window under `base-benchmark`'s runner: the start marker, acknowledged before any request planned in the window reaches the server; the stop marker; and the record after both, on stdout. The runner is played by the test, which logs when it would take its snapshot of the services, in one log with the server's arrivals, so that the order of the two is asserted from events alone.

use super::{
    count, extension, load_with_window,
    server::{Action, Arrival, HELLO, Server},
};
use base_benchmark::{
    CONTROL_STDIO, CONTROL_VARIABLE, Measurement, ProcessRecord, WINDOW_START, WINDOW_STARTED,
    WINDOW_STOP, Window,
};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// A request the server read.
    Request(usize, usize),
    /// The runner's snapshot at the start marker, before it acknowledges it.
    Started,
    Stopped,
}

type Log = Arc<Mutex<Vec<Event>>>;

/// A server that answers every request with `HELLO`, logging each, and corrupting the body of every load request for which `corrupt` holds.
fn logging(log: &Log, corrupt: fn(Arrival) -> bool) -> Server {
    let log = log.clone();
    Server::start(move |arrival| {
        log.lock()
            .unwrap()
            .push(Event::Request(arrival.connection, arrival.request));
        let mut response = HELLO.to_vec();
        if arrival.connection > 0 && corrupt(arrival) {
            *response.last_mut().unwrap() = b'?';
        }
        Action::Respond(response)
    })
}

/// Where in `log` the runner took its snapshot at the start marker.
fn started(log: &[Event]) -> usize {
    let started: Vec<usize> = (0..log.len())
        .filter(|index| log[*index] == Event::Started)
        .collect();
    assert_eq!(started.len(), 1, "{log:?}");
    started[0]
}

/// Assert that the load's only connection had none of its requests from ordinal `first` on, those counted in the window, read before the runner's snapshot, and that it had some read after it.
fn counted_after_the_snapshot(log: &[Event], first: usize) {
    let started = started(log);
    let counted = |event: &Event| matches!(event, Event::Request(1, request) if *request >= first);
    assert!(!log[..started].iter().any(counted), "{log:?}");
    assert!(log[started..].iter().any(counted), "{log:?}");
    // The preflight request, on its own connection, comes before any load.
    assert_eq!(log[0], Event::Request(0, 0));
}

/// The window's markers, as the runner reads them: the start marker logs the runner's snapshot. With `fail_stop`, writing the stop marker fails, as when the runner has gone.
struct Markers {
    log: Log,
    fail_stop: bool,
}

impl Write for Markers {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let line = std::str::from_utf8(bytes).unwrap();
        let event = match line.strip_suffix('\n') {
            Some(WINDOW_START) => Event::Started,
            Some(WINDOW_STOP) if self.fail_stop => {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the runner left"));
            }
            Some(WINDOW_STOP) => Event::Stopped,
            _ => panic!("expected a whole marker line, not {line:?}"),
        };
        self.log.lock().unwrap().push(event);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn markers(log: &Log) -> Markers {
    Markers {
        log: log.clone(),
        fail_stop: false,
    }
}

/// The runner's acknowledgement, which it writes only once `delay` has passed after the driver began to wait for it.
struct Acknowledgement {
    delay: Duration,
    sent: bool,
}

impl Read for Acknowledgement {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.sent {
            return Ok(0);
        }
        std::thread::sleep(self.delay);
        self.sent = true;
        let line = format!("{WINDOW_STARTED}\n");
        buffer[..line.len()].copy_from_slice(line.as_bytes());
        Ok(line.len())
    }
}

fn acknowledgement(delay: Duration) -> BufReader<Acknowledgement> {
    BufReader::new(Acknowledgement { delay, sent: false })
}

/// The binary under a runner that speaks the stdio control protocol: the markers are the first two lines of its stdout, start then stop, and the record is the third and last; the start is acknowledged, and the runner's snapshot taken, before any load request reaches the server. The operations are the valid responses only, and the invalid ones are the total that a scenario declares with `invalid-responses = "responses_invalid"`.
#[test]
fn the_binary_marks_its_window_before_its_record() {
    let log = Log::default();
    let server = logging(&log, |arrival| arrival.request % 2 == 1);
    let address = server.address.to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_base-benchmark-load"))
        .args([
            "rate",
            "--address",
            &address,
            "--rate",
            "1000",
            "--connections",
            "1",
            "--warmup",
            "0",
            "--duration",
            "50ms",
        ])
        .env(CONTROL_VARIABLE, CONTROL_STDIO)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = Some(child.stdin.take().unwrap());
    let mut lines = Vec::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        match line.as_str() {
            WINDOW_START => {
                log.lock().unwrap().push(Event::Started);
                // The acknowledgement is the only line the driver reads, and its stdin then closes, as the runner's does.
                let mut input = input.take().expect("the start is marked once");
                writeln!(input, "{WINDOW_STARTED}").unwrap();
            }
            WINDOW_STOP => log.lock().unwrap().push(Event::Stopped),
            _ => {}
        }
        lines.push(line);
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[..2], [WINDOW_START, WINDOW_STOP]);
    let record: ProcessRecord = serde_json::from_str(&lines[2]).unwrap();
    record.validate().unwrap();
    // Without a warmup, the start is marked before the load starts, so no request but the preflight comes before the snapshot.
    let log = log.lock().unwrap();
    counted_after_the_snapshot(&log, 0);
    let window = &extension(&record)["window"];
    assert_eq!(window["control"], "stdio");
    assert_eq!(window["before_start"], true);
    // The load starts 100 ms after the acknowledgement.
    assert!(window["acknowledged_before_ns"].as_u64().unwrap() >= 100_000_000);
    assert!(stderr.contains("window marked for base-benchmark: start acknowledged in"));
    assert!(stderr.contains("before the load started"));
    // Every other one of the 50 responses of the window is invalid: they are not operations, and the invalid ones are a total.
    assert_eq!(record.samples[0].operations, 25);
    assert_eq!(count(&record, "responses_valid"), 25);
    assert_eq!(
        record.samples[0]
            .metric("responses_invalid")
            .unwrap()
            .measurement,
        Measurement::Total { value: 25 }
    );
}

/// A control the tool does not know fails the run without a record, rather than running unmarked.
#[test]
fn an_unknown_control_fails_without_a_record() {
    let output = Command::new(env!("CARGO_BIN_EXE_base-benchmark-load"))
        .args(["closed", "--address", "127.0.0.1:1"])
        .env(CONTROL_VARIABLE, "file")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("supports only \"stdio\""), "{stderr}");
}

/// With a warmup longer than `--window-lead`, the start is marked during the warmup and acknowledged before the window starts, so the warmup's requests may come before the snapshot, but none counted in the window does. The lead is only a watchdog here, seconds long: the acknowledgement comes as soon as the start is marked.
#[test]
fn a_start_marked_in_the_warmup_is_acknowledged_before_the_window() {
    let log = Log::default();
    let server = logging(&log, |_| false);
    let mut window = Window::new(markers(&log), acknowledgement(Duration::ZERO));
    let report = load_with_window(
        "rate",
        server.address,
        &[
            "--rate",
            "200",
            "--connections",
            "1",
            "--warmup",
            "3s",
            "--window-lead",
            "2s",
            "--duration",
            "100ms",
            "--quiet",
        ],
        Some(&mut window),
    )
    .unwrap();
    let record = report.record().unwrap();
    // The window holds requests 600 to 619.
    assert_eq!(record.samples[0].operations, 20);
    let log = log.lock().unwrap();
    counted_after_the_snapshot(&log, 600);
    assert_eq!(log.last(), Some(&Event::Stopped));
    let window = &extension(&record)["window"];
    assert_eq!(window["before_start"], false);
    assert_eq!(window["lead_ns"], 2_000_000_000u64);
    assert!(report.summary().contains("before the window; stop marked"));
}

/// An acknowledgement that comes after the window has started fails the run, once its requests have outcomes, since the runner's snapshot may follow counted requests. The runner here acknowledges only 50 ms after the start was marked, which was at most the 1 ms lead before the window.
#[test]
fn a_late_acknowledgement_fails_the_run() {
    let log = Log::default();
    let server = logging(&log, |_| false);
    let mut window = Window::new(markers(&log), acknowledgement(Duration::from_millis(50)));
    let error = load_with_window(
        "rate",
        server.address,
        &[
            "--rate",
            "1000",
            "--connections",
            "1",
            "--warmup",
            "20ms",
            "--window-lead",
            "1ms",
            "--duration",
            "10ms",
            "--quiet",
        ],
        Some(&mut window),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let message = error.to_string();
    assert!(
        message.starts_with("base-benchmark acknowledged the window's start "),
        "{message}"
    );
    assert!(message.ends_with("after the window began, so its snapshots of the services may follow counted requests; lengthen --window-lead, now 1.000 ms"), "{message}");
    // The stop is not marked for a run that fails.
    assert!(!log.lock().unwrap().contains(&Event::Stopped));
}

/// A runner that closes stdin without acknowledging the start fails the run. Marked before the load starts, the workers are released without sending a request; marked during the warmup, the run fails once its requests have outcomes.
#[test]
fn a_missing_acknowledgement_fails_the_run() {
    for (warmup, lead) in [("0", "10ms"), ("20ms", "1ms")] {
        let log = Log::default();
        let server = logging(&log, |_| false);
        let mut window = Window::new(markers(&log), io::empty());
        let error = load_with_window(
            "rate",
            server.address,
            &[
                "--rate",
                "1000",
                "--connections",
                "1",
                "--warmup",
                warmup,
                "--window-lead",
                lead,
                "--duration",
                "10ms",
                "--quiet",
            ],
            Some(&mut window),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "marking the window's start for base-benchmark: expected the runner to acknowledge the window's start, but read \"\""
        );
        if warmup == "0" {
            assert_eq!(*log.lock().unwrap(), [Event::Request(0, 0), Event::Started]);
        }
    }
}

/// A stop marker that cannot be written fails the run.
#[test]
fn a_stop_marker_that_cannot_be_sent_fails_the_run() {
    let log = Log::default();
    let server = logging(&log, |_| false);
    let output = Markers {
        log: log.clone(),
        fail_stop: true,
    };
    let mut window = Window::new(output, acknowledgement(Duration::ZERO));
    let error = load_with_window(
        "rate",
        server.address,
        &[
            "--rate",
            "1000",
            "--connections",
            "1",
            "--warmup",
            "0",
            "--duration",
            "10ms",
            "--quiet",
        ],
        Some(&mut window),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(
        error.to_string(),
        "marking the window's stop for base-benchmark: the runner left"
    );
}
