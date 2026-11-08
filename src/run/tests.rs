// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::clock::StepClock;

fn expectation(body: Option<&[u8]>, response: Option<&[u8]>) -> Expectation {
    Expectation {
        status: 200,
        headers: Vec::new(),
        body: body.map(<[u8]>::to_vec),
        response: response.map(<[u8]>::to_vec),
    }
}

#[test]
fn signatures_identify_valid_responses() {
    assert_eq!(
        signature(&expectation(Some(b"ok"), None), 200),
        format!("http1 status 200 body 2:{}", digest(b"ok"))
    );
    assert_eq!(
        signature(&expectation(None, None), 204),
        "http1 status 204 body any"
    );
    assert_eq!(
        signature(&expectation(Some(b"ok"), Some(b"raw")), 200),
        format!("http1 response 3:{}", digest(b"raw"))
    );
}

#[test]
fn parameters_describe_the_work() {
    let mut options = Options::new(Mode::Rate);
    options.rate = std::num::NonZeroU64::new(5);
    options.expect_headers = vec![("Content-Type".to_owned(), "text/plain".to_owned())];
    let request = RequestTemplate::new("GET", "/", "host", &[], b"");
    let expectation = expectation(None, None);
    let described = parameters(&options, &request, &expectation);
    assert_eq!(described["rate"], 5);
    assert_eq!(described["arrivals"], "uniform");
    assert_eq!(described["assignment"], "per-connection");
    assert_eq!(described["validation"]["body"], "preflight");
    assert_eq!(
        described["validation"]["headers"],
        json!([["content-type", "text/plain"]])
    );
    assert_eq!(described["request"]["host"], "default");
    assert_eq!(
        described["request"]["digest"],
        digest(b"GET / HTTP/1.1\r\n\r\n")
    );
    assert_eq!(described["request"]["length"], 18);
    // The address varies between equal runs, such as runs against servers on different ports, so the default Host header that names it is not a parameter.
    let elsewhere = RequestTemplate::new("GET", "/", "127.0.0.1:9999", &[], b"");
    assert_eq!(parameters(&options, &elsewhere, &expectation), described);
    // A Host header given explicitly changes the request, so it is.
    let headers = [("Host".to_owned(), "example".to_owned())];
    let explicit = RequestTemplate::new("GET", "/", "host", &headers, b"");
    let explicit = parameters(&options, &explicit, &expectation);
    assert_eq!(explicit["request"]["host"], "header");
    assert_eq!(
        explicit["request"]["digest"],
        digest(b"GET / HTTP/1.1\r\nHost: example\r\n\r\n")
    );
    options.body = BodyExpectation::Any;
    assert_eq!(
        parameters(&options, &request, &expectation)["validation"]["body"],
        "any"
    );
    options.body = BodyExpectation::Exact(b"ok".to_vec());
    assert_eq!(
        parameters(&options, &request, &expectation)["validation"]["body"],
        json!({"length": 2, "digest": digest(b"ok")})
    );
    options.response = Some(b"raw".to_vec());
    assert_eq!(
        parameters(&options, &request, &expectation)["validation"]["body"],
        "response"
    );
}

/// A reader that is interrupted by a signal `interruptions` times before every chunk it returns, and records the read timeouts it is given.
struct Interrupted<'a> {
    chunks: std::collections::VecDeque<&'a [u8]>,
    interruptions: usize,
    interrupted: usize,
    timeouts: std::cell::RefCell<Vec<Duration>>,
    /// Whether setting a timeout fails.
    refuse_timeouts: bool,
}

impl<'a> Interrupted<'a> {
    fn new(chunks: &[&'a [u8]], interruptions: usize) -> Self {
        Self {
            chunks: chunks.iter().copied().collect(),
            interruptions,
            interrupted: 0,
            timeouts: Vec::new().into(),
            refuse_timeouts: false,
        }
    }
}

impl Read for Interrupted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.interrupted < self.interruptions {
            self.interrupted += 1;
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.interrupted = 0;
        let chunk = self.chunks.pop_front().unwrap_or_default();
        buffer[..chunk.len()].copy_from_slice(chunk);
        Ok(chunk.len())
    }
}

impl TimedRead for &mut Interrupted<'_> {
    fn set_read_timeout(&self, timeout: Duration) -> io::Result<()> {
        if self.refuse_timeouts {
            return Err(io::Error::other("no timeouts"));
        }
        self.timeouts.borrow_mut().push(timeout);
        Ok(())
    }
}

#[test]
fn preflight_reads_are_retried_when_interrupted() {
    let request = RequestTemplate::new("GET", "/", "host", &[], b"");
    let mut reader = Interrupted::new(
        &[&b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no"[..], b"k"],
        1,
    );
    // Each clock read advances 1 µs: an interrupted read is retried with what remains of its timeout, and a read that returns data starts the next one's afresh.
    let clock = StepClock::new(0, 1_000);
    let response = read_response(
        &mut reader,
        10_000,
        &clock,
        &request,
        &expectation(None, None),
    )
    .unwrap();
    assert_eq!(response.body.as_deref(), Some(&b"ok"[..]));
    assert_eq!(
        *reader.timeouts.borrow(),
        [10, 9, 10, 9].map(Duration::from_micros)
    );
}

/// A signal arriving faster than the read timeout interrupts every read, and the read still times out once its whole timeout has passed.
#[test]
fn preflight_reads_time_out_however_often_they_are_interrupted() {
    let request = RequestTemplate::new("GET", "/", "host", &[], b"");
    let mut reader = Interrupted::new(&[], usize::MAX);
    let clock = StepClock::new(0, 1_000);
    let error = read_response(
        &mut reader,
        3_000,
        &clock,
        &request,
        &expectation(None, None),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "the server sent nothing more in time");
    assert_eq!(
        *reader.timeouts.borrow(),
        [3, 2, 1].map(Duration::from_micros)
    );
}

#[test]
fn preflight_reads_fail_when_their_timeout_cannot_be_set() {
    let request = RequestTemplate::new("GET", "/", "host", &[], b"");
    let mut reader = Interrupted::new(&[], 0);
    reader.refuse_timeouts = true;
    let clock = StepClock::new(0, 1_000);
    let error = read_response(
        &mut reader,
        3_000,
        &clock,
        &request,
        &expectation(None, None),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "no timeouts");
}

/// A worker that panics before it reports its setup fails the run with an error naming it, instead of leaving the controller waiting for it, and the workers that did set up are abandoned.
#[test]
fn a_worker_that_panics_while_setting_up_fails_the_run() {
    let abandoned = std::sync::Mutex::new(Vec::new());
    let error = drive(
        2,
        None,
        0,
        0,
        &MonotonicClock::new(),
        |worker, start| {
            let arrival = start.arrival(worker);
            if worker == 1 {
                panic!("injected failure");
            }
            let started = arrival.ready(Ok(()));
            abandoned.lock().unwrap().push((worker, started));
            None
        },
        |_, builder| Ok(builder),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "worker 1 panicked while setting up");
    assert_eq!(*abandoned.lock().unwrap(), [(0, None)]);
}

/// A worker thread that cannot be spawned, as when the process may have no more threads, fails the run with an error naming how many workers started, instead of leaving them waiting for the others at the start; they are abandoned and joined, and no other is spawned. The failure to spawn is reported even when a worker that started also failed to set up.
#[test]
fn a_worker_that_cannot_be_spawned_fails_the_run() {
    for failing in [0, 2] {
        let abandoned = std::sync::Mutex::new(Vec::new());
        let error = drive(
            4,
            None,
            0,
            0,
            &MonotonicClock::new(),
            |worker, start| {
                let setup = match worker {
                    1 => Err(io::Error::other("injected setup failure")),
                    _ => Ok(()),
                };
                let started = start.arrival(worker).ready(setup);
                abandoned.lock().unwrap().push((worker, started));
                None
            },
            |worker, builder| match worker == failing {
                true => Err(io::Error::from_raw_os_error(libc::EAGAIN)),
                false => Ok(builder),
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            error.to_string(),
            format!(
                "spawning worker {failing} failed, with {failing} of 4 workers started: {}",
                io::Error::from_raw_os_error(libc::EAGAIN)
            )
        );
        let mut abandoned = abandoned.into_inner().unwrap();
        abandoned.sort_by_key(|(worker, _)| *worker);
        let expected: Vec<_> = (0..failing).map(|worker| (worker, None)).collect();
        assert_eq!(abandoned, expected);
    }
}

#[test]
fn sleeping_until_a_past_instant_returns_at_once() {
    let clock = MonotonicClock::new();
    sleep_until(&clock, 0);
    let target = clock.now() + 1_000_000;
    sleep_until(&clock, target);
    assert!(clock.now() >= target);
}

#[test]
fn addresses_resolve() {
    assert_eq!(
        resolve("127.0.0.1:80").unwrap(),
        "127.0.0.1:80".parse::<SocketAddr>().unwrap()
    );
    assert!(resolve("localhost:80").unwrap().ip().is_loopback());
}
