// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::{clock::MonotonicClock, poller::Epoll, statistics::Period};
use std::{net::TcpListener, num::NonZeroU64, os::fd::RawFd};

const MS: u64 = 1_000_000;

struct Fixture {
    request: RequestTemplate,
    expectation: Expectation,
}

impl Fixture {
    fn new() -> Self {
        Self {
            request: RequestTemplate::new("GET", "/", "test", &[], b""),
            expectation: Expectation {
                status: 200,
                ..Expectation::default()
            },
        }
    }

    /// An open loop of `connections` connections on one worker at 1,000 requests per second, with no warmup and a 10 ms window.
    fn load(&self, address: SocketAddr, connections: usize) -> Load<'_> {
        Load {
            address,
            connections,
            workers: 1,
            cpus: &[],
            depth: 1,
            schedule: Some(Schedule::new(NonZeroU64::new(1000).unwrap())),
            warmup: 0,
            duration: 10 * MS,
            timeout: 100 * MS,
            connect_timeout: 10_000 * MS,
            drain_timeout: 1000 * MS,
            reconnect_delay: MS,
            interval: 10 * MS,
            request: &self.request,
            expectation: &self.expectation,
        }
    }
}

/// A poller that refuses registrations after `allowed` of them.
struct Refusing {
    epoll: Epoll,
    allowed: usize,
}

impl Poller for Refusing {
    fn add(&mut self, descriptor: RawFd, token: u64) -> io::Result<()> {
        if self.allowed == 0 {
            return Err(io::Error::other("registration refused"));
        }
        self.allowed -= 1;
        self.epoll.add(descriptor, token)
    }

    fn delete(&mut self, descriptor: RawFd) -> io::Result<()> {
        self.epoll.delete(descriptor)
    }

    fn wait(&mut self, events: &mut [Event], timeout: i32) -> io::Result<usize> {
        self.epoll.wait(events, timeout)
    }
}

/// A poller that reports every event twice, as a level change after an edge might.
struct Duplicating(Epoll);

impl Poller for Duplicating {
    fn add(&mut self, descriptor: RawFd, token: u64) -> io::Result<()> {
        self.0.add(descriptor, token)
    }

    fn delete(&mut self, descriptor: RawFd) -> io::Result<()> {
        self.0.delete(descriptor)
    }

    fn wait(&mut self, events: &mut [Event], timeout: i32) -> io::Result<usize> {
        let count = self.0.wait(&mut events[..EVENT_CAPACITY / 2], timeout)?;
        for index in (0..count).rev() {
            events[2 * index] = events[index];
            events[2 * index + 1] = events[index];
        }
        Ok(2 * count)
    }
}

fn connections(count: usize) -> Vec<Connection<TcpStream>> {
    (0..count)
        .map(|index| Connection::new(index as u64))
        .collect()
}

fn listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").unwrap()
}

#[test]
fn the_start_waits_for_every_worker() {
    let start = Start::new(1);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| start.ready(Ok(())));
        assert!(start.wait_for_workers().is_none());
        start.publish(42);
        assert_eq!(worker.join().unwrap(), Some(42));
    });
    let start = Start::new(2);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| start.ready(Err(io::Error::other("first"))));
        let second = scope.spawn(|| start.ready(Ok(())));
        assert_eq!(start.wait_for_workers().unwrap().to_string(), "first");
        start.abort();
        assert_eq!(
            (first.join().unwrap(), second.join().unwrap()),
            (None, None)
        );
    });
}

#[test]
fn workers_that_fail_to_set_up_return_nothing() {
    let fixture = Fixture::new();
    let (_socket, address) = crate::os::refusing();
    let load = fixture.load(address, 1);
    let start = Start::new(1);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            work::<Epoll, _>(
                &load,
                0,
                Err(io::Error::other("no poller")),
                &MonotonicClock::new(),
                &start,
            )
        });
        assert_eq!(start.wait_for_workers().unwrap().to_string(), "no poller");
        start.abort();
        assert!(worker.join().unwrap().is_none());
    });
}

#[test]
fn connecting_reports_immediate_failures() {
    let fixture = Fixture::new();
    let load = fixture.load("224.0.0.1:80".parse().unwrap(), 1);
    let error = connect(
        &load,
        &mut connections(1),
        &mut Epoll::new().unwrap(),
        &MonotonicClock::new(),
    )
    .unwrap_err();
    assert_eq!(error.raw_os_error(), None);
    assert!(
        error
            .to_string()
            .starts_with("connecting to 224.0.0.1:80: "),
        "{error}"
    );
}

#[test]
fn connecting_reports_registration_failures() {
    let fixture = Fixture::new();
    let listener = listener();
    let load = fixture.load(listener.local_addr().unwrap(), 2);
    let mut poller = Refusing {
        epoll: Epoll::new().unwrap(),
        allowed: 1,
    };
    let error = connect(
        &load,
        &mut connections(2),
        &mut poller,
        &MonotonicClock::new(),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "registration refused");
}

#[test]
fn connecting_reports_refusals() {
    let fixture = Fixture::new();
    let (_socket, address) = crate::os::refusing();
    let load = fixture.load(address, 1);
    let error = connect(
        &load,
        &mut connections(1),
        &mut Epoll::new().unwrap(),
        &MonotonicClock::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
}

#[test]
fn connecting_times_out() {
    let fixture = Fixture::new();
    let listener = listener();
    let mut load = fixture.load(listener.local_addr().unwrap(), 1);
    load.connect_timeout = 0;
    let error = connect(
        &load,
        &mut connections(1),
        &mut Epoll::new().unwrap(),
        &MonotonicClock::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        error
            .to_string()
            .ends_with("1 connections were not established in time"),
        "{error}"
    );
}

#[test]
fn connecting_ignores_repeated_readiness() {
    let fixture = Fixture::new();
    let listener = listener();
    let load = fixture.load(listener.local_addr().unwrap(), 3);
    let mut connections = connections(3);
    connect(
        &load,
        &mut connections,
        &mut Duplicating(Epoll::new().unwrap()),
        &MonotonicClock::new(),
    )
    .unwrap();
    assert!(
        connections
            .iter()
            .all(|connection| connection.state == State::Open)
    );
}

#[test]
fn sends_to_a_reset_connection_fail() {
    let fixture = Fixture::new();
    let listener = listener();
    let load = fixture.load(listener.local_addr().unwrap(), 1);
    let settings = load.settings(MS);
    let mut statistics = WorkerStatistics::new(&settings.timing);
    let mut connection = Connection::new(0);
    connection.connecting(TcpStream::connect(load.address).unwrap(), 0);
    connection.connected(0, &mut statistics);
    let (accepted, _) = listener.accept().unwrap();
    // Close with a reset, and wait until the client has received it.
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: The descriptor is open and `linger` is a valid option value of its size.
    unsafe {
        libc::setsockopt(
            accepted.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&raw const linger).cast(),
            size_of::<libc::linger>() as libc::socklen_t,
        );
    }
    drop(accepted);
    let mut descriptor = libc::pollfd {
        fd: connection.stream.as_ref().unwrap().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: One valid descriptor; the timeout is only a watchdog.
    assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 10_000) }, 1);
    let mut poller = Epoll::new().unwrap();
    poller
        .add(connection.stream.as_ref().unwrap().as_raw_fd(), 0)
        .unwrap();
    send(
        &mut connection,
        &mut poller,
        2 * MS,
        1,
        &settings,
        &mut statistics,
    );
    assert_eq!(
        statistics.period(Period::Measured).errors[ErrorKind::Reset as usize],
        1
    );
    assert!(matches!(connection.state, State::Down { .. }));
}

#[test]
fn maintenance_times_out_connections_and_retries_failed_ones() {
    let fixture = Fixture::new();
    let load = fixture.load("224.0.0.1:80".parse().unwrap(), 3);
    let settings = load.settings(MS);
    let mut statistics = WorkerStatistics::new(&settings.timing);
    let mut connections = connections(3);
    connections[0].state = State::Connecting { since: 0 };
    connections[1].state = State::Down { until: 0 };
    connections[2].state = State::Down { until: u64::MAX };
    let mut poller = Epoll::new().unwrap();
    let now = load.connect_timeout;
    assert!(maintain(
        &load,
        &mut connections,
        &mut poller,
        now,
        &settings,
        &mut statistics
    ));
    // The attempt in progress timed out, and the retry failed at once; the third waits.
    assert_eq!(statistics.connect_errors, 2);
    assert_eq!(connections[0].state, State::Down { until: now + MS });
    assert_eq!(connections[1].state, State::Down { until: now + MS });
    assert!(!maintain(
        &load,
        &mut connections,
        &mut poller,
        now,
        &settings,
        &mut statistics
    ));
}

#[test]
fn the_drain_deadline_abandons_what_is_left() {
    let fixture = Fixture::new();
    let mut load = fixture.load("127.0.0.1:1".parse().unwrap(), 2);
    load.schedule = None;
    let settings = load.settings(MS);
    let mut statistics = WorkerStatistics::new(&settings.timing);
    let mut connections = connections(2);
    let mut poller = Epoll::new().unwrap();
    assert!(!done(
        &mut connections,
        &mut poller,
        settings.timing.window_end - 1,
        0,
        &settings,
        &mut statistics
    ));
    assert!(done(
        &mut connections,
        &mut poller,
        settings.timing.window_end,
        0,
        &settings,
        &mut statistics
    ));
    assert!(done(
        &mut connections,
        &mut poller,
        settings.drain_deadline,
        0,
        &settings,
        &mut statistics
    ));
    assert!(
        connections
            .iter()
            .all(|connection| connection.state == State::Down { until: u64::MAX })
    );
    assert_eq!(statistics.finished(), 0);
}
