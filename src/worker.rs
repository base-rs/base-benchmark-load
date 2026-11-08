// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{
    clock::Clock,
    connection::{Connection, Settings, State},
    os,
    poller::{Event, Poller, interrupted},
    request::RequestTemplate,
    response::Expectation,
    schedule::{Cursor, Schedule, count_below},
    statistics::{ConnectionCounters, ErrorKind, Timing, WorkerStatistics},
};
use std::{
    io,
    net::{SocketAddr, TcpStream},
    os::fd::AsRawFd,
    sync::{
        Barrier, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

/// How many events one poll returns at most.
const EVENT_CAPACITY: usize = 256;
/// How often workers check for timeouts, reconnect, and decide whether they are done.
const MAINTENANCE_PERIOD: u64 = 1_000_000;

/// The load every worker shares.
#[derive(Debug)]
pub struct Load<'a> {
    pub address: SocketAddr,
    pub connections: usize,
    pub workers: usize,
    /// The CPU of each worker, or none to leave them unpinned.
    pub cpus: &'a [usize],
    pub depth: usize,
    /// The open-loop schedule, or `None` for the closed loop.
    pub schedule: Option<Schedule>,
    pub warmup: u64,
    pub duration: u64,
    pub timeout: u64,
    pub connect_timeout: u64,
    pub drain_timeout: u64,
    pub reconnect_delay: u64,
    pub interval: u64,
    pub request: &'a RequestTemplate,
    pub expectation: &'a Expectation,
}

impl Load<'_> {
    /// The settings of a run starting at `start`.
    pub fn settings(&self, start: u64) -> Settings<'_> {
        let window_start = start.saturating_add(self.warmup);
        let window_end = window_start.saturating_add(self.duration);
        let ordinal = |offset| {
            self.schedule
                .map_or(0, |schedule| schedule.first_at_or_after(offset))
        };
        Settings {
            timing: Timing {
                closed: self.schedule.is_none(),
                start,
                window_start,
                window_end,
                interval: self.interval,
            },
            schedule: self.schedule,
            total: ordinal(self.warmup.saturating_add(self.duration)),
            measured_first: ordinal(self.warmup),
            depth: self.depth,
            connections: self.connections as u64,
            timeout: self.timeout,
            connect_timeout: self.connect_timeout,
            reconnect_delay: self.reconnect_delay,
            drain_deadline: window_end.saturating_add(self.drain_timeout),
            request: &self.request.bytes,
            head: self.request.head,
            expectation: self.expectation,
        }
    }
}

/// The value of [`Start`]'s instant when the run is abandoned before it starts.
const ABORT: u64 = u64::MAX;

/// Starts every worker at the same instant, once all have connected.
#[derive(Debug)]
pub struct Start {
    barrier: Barrier,
    failure: Mutex<Option<io::Error>>,
    instant: AtomicU64,
}

impl Start {
    pub fn new(workers: usize) -> Self {
        Self {
            barrier: Barrier::new(workers + 1),
            failure: Mutex::new(None),
            instant: AtomicU64::new(0),
        }
    }

    /// Report a worker's setup, wait for the others, and return the start, or `None` if the run was abandoned.
    fn ready(&self, setup: io::Result<()>) -> Option<u64> {
        if let Err(error) = setup {
            self.failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert(error);
        }
        self.barrier.wait();
        loop {
            match self.instant.load(Ordering::Acquire) {
                0 => std::hint::spin_loop(),
                ABORT => return None,
                start => return Some(start),
            }
        }
    }

    /// Wait for every worker's setup, and return the first failure.
    pub fn wait_for_workers(&self) -> Option<io::Error> {
        self.barrier.wait();
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Start the workers at `start`, which is positive.
    pub fn publish(&self, start: u64) {
        self.instant.store(start, Ordering::Release);
    }

    pub fn abort(&self) {
        self.publish(ABORT);
    }
}

/// What one worker measured, and each of its connections' counters.
#[derive(Debug)]
pub struct WorkerResult {
    pub statistics: WorkerStatistics,
    pub connections: Vec<ConnectionCounters>,
}

/// Run worker `worker`: pin it, connect its share of the connections, wait for the start, then send and receive until every request has an outcome. Returns `None` when the run was abandoned before the start, because this or another worker failed to set up.
pub fn work<P: Poller, C: Clock>(
    load: &Load,
    worker: usize,
    poller: io::Result<P>,
    clock: &C,
    start: &Start,
) -> Option<WorkerResult> {
    let mut connections: Vec<Connection<TcpStream>> = (worker..load.connections)
        .step_by(load.workers)
        .map(|index| Connection::new(index as u64))
        .collect();
    let setup = poller.and_then(|mut poller| {
        if let Some(cpu) = load.cpus.get(worker) {
            os::pin(*cpu)?;
        }
        connect(load, &mut connections, &mut poller, clock)?;
        Ok(poller)
    });
    let (setup, poller) = match setup {
        Ok(poller) => (Ok(()), Some(poller)),
        Err(error) => (Err(error), None),
    };
    let start = start.ready(setup)?;
    let mut poller = poller.expect("the run starts only when every worker set up");
    let settings = load.settings(start);
    let mut statistics = WorkerStatistics::new(&settings.timing);
    statistics.connects = connections.len() as u64;
    let planned: u64 = (worker..load.connections)
        .step_by(load.workers)
        .map(|index| count_below(index as u64, settings.connections, settings.total))
        .sum();
    let mut cursor = Cursor::new(worker, load.workers, load.connections);
    // When the cursor's ordinal is due, computed once per ordinal rather than on every poll; never in the closed loop.
    let due_time = |cursor: &Cursor| match settings.schedule {
        Some(schedule) if cursor.ordinal() < settings.total => {
            start + schedule.offset(cursor.ordinal())
        }
        _ => u64::MAX,
    };
    let mut next_due = due_time(&cursor);
    let mut events = [Event::default(); EVENT_CAPACITY];
    while clock.now() < start {
        std::hint::spin_loop();
    }
    if settings.schedule.is_none() {
        for connection in &mut connections {
            send(
                connection,
                &mut poller,
                clock.now(),
                u64::MAX,
                &settings,
                &mut statistics,
            );
        }
    }
    let window = settings.timing.window_start..settings.timing.window_end;
    let (mut cpu_start, mut cpu_end) = (None, None);
    let mut previous = clock.now();
    let mut worked = false;
    let mut maintenance = previous;
    loop {
        let now = clock.now();
        if window.contains(&previous) {
            statistics.iterations += 1;
            if worked {
                statistics.busy += now - previous;
            }
        }
        if cpu_start.is_none() && now >= window.start {
            cpu_start = Some(os::thread_cpu_time());
        }
        if cpu_end.is_none() && now >= window.end {
            cpu_end = Some(os::thread_cpu_time());
        }
        previous = now;
        let due = cursor.ordinal().min(settings.total);
        let count = poller
            .wait(&mut events, 0)
            .or_else(interrupted)
            .expect("polling a valid poller succeeds");
        worked = count > 0;
        for event in &events[..count] {
            handle(
                &mut connections[event.token as usize],
                &mut poller,
                event,
                clock,
                due,
                &settings,
                &mut statistics,
            );
        }
        while now >= next_due {
            let local = cursor.local();
            cursor.advance();
            next_due = due_time(&cursor);
            let due = cursor.ordinal().min(settings.total);
            send(
                &mut connections[local],
                &mut poller,
                clock.now(),
                due,
                &settings,
                &mut statistics,
            );
            worked = true;
        }
        if now >= maintenance {
            maintenance = now + MAINTENANCE_PERIOD;
            worked |= maintain(
                load,
                &mut connections,
                &mut poller,
                now,
                &settings,
                &mut statistics,
            );
            if done(
                &mut connections,
                &mut poller,
                now,
                planned,
                &settings,
                &mut statistics,
            ) {
                break;
            }
        }
    }
    let cpu_end = cpu_end.unwrap_or_else(os::thread_cpu_time);
    statistics.cpu = cpu_end.saturating_sub(cpu_start.unwrap_or(cpu_end));
    Some(WorkerResult {
        statistics,
        connections: connections
            .iter()
            .map(|connection| connection.counters)
            .collect(),
    })
}

/// Connect every connection before the start, failing if any connection fails or the connect timeout passes.
fn connect<P: Poller, C: Clock>(
    load: &Load,
    connections: &mut [Connection<TcpStream>],
    poller: &mut P,
    clock: &C,
) -> io::Result<()> {
    let context = |error: io::Error| {
        io::Error::new(
            error.kind(),
            format!("connecting to {}: {error}", load.address),
        )
    };
    for (local, connection) in connections.iter_mut().enumerate() {
        let stream = os::connect(load.address).map_err(context)?;
        poller.add(stream.as_raw_fd(), local as u64)?;
        connection.connecting(stream, clock.now());
    }
    let deadline = clock.now().saturating_add(load.connect_timeout);
    let mut pending = connections.len();
    let mut events = [Event::default(); EVENT_CAPACITY];
    while pending > 0 {
        if clock.now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "connecting to {}: {pending} connections were not established in time",
                    load.address
                ),
            ));
        }
        let count = poller
            .wait(&mut events, 1)
            .or_else(interrupted)
            .expect("polling a valid poller succeeds");
        for event in &events[..count] {
            let connection = &mut connections[event.token as usize];
            if matches!(connection.state, State::Connecting { .. }) {
                let stream = connection
                    .stream
                    .as_ref()
                    .expect("connecting connections have a stream");
                // The socket's pending error, or the error reading it.
                if let Some(error) = stream.take_error().unwrap_or_else(Some) {
                    return Err(context(error));
                }
                connection.state = State::Open;
                pending -= 1;
            }
        }
    }
    Ok(())
}

/// Remove a connection's socket from the poller before the connection closes it, so that no event for it can follow.
fn deregister<P: Poller>(connection: &Connection<TcpStream>, poller: &mut P) {
    if let Some(stream) = &connection.stream {
        // Only registered sockets are stored, so this cannot fail.
        let _ = poller.delete(stream.as_raw_fd());
    }
}

/// Queue what `connection` may send and write it.
fn send<P: Poller>(
    connection: &mut Connection<TcpStream>,
    poller: &mut P,
    now: u64,
    due: u64,
    settings: &Settings,
    statistics: &mut WorkerStatistics,
) {
    if connection.dispatch(now, due, settings, statistics) > 0 {
        if let Err(kind) = connection.flush(statistics) {
            deregister(connection, poller);
            connection.fail(kind, now, settings, statistics);
        }
    }
}

fn handle<P: Poller>(
    connection: &mut Connection<TcpStream>,
    poller: &mut P,
    event: &Event,
    clock: &impl Clock,
    due: u64,
    settings: &Settings,
    statistics: &mut WorkerStatistics,
) {
    match connection.state {
        State::Connecting { .. } => {
            let now = clock.now();
            let stream = connection
                .stream
                .as_ref()
                .expect("connecting connections have a stream");
            if let Ok(None) = stream.take_error() {
                connection.connected(now, statistics);
                send(connection, poller, now, due, settings, statistics);
            } else {
                deregister(connection, poller);
                connection.connect_failed(now, settings, statistics);
            }
        }
        State::Open => {
            let mut result = Ok(());
            if event.readable || event.closed {
                result = connection.readable(clock, settings, statistics);
            }
            let result = result.and_then(|()| {
                connection.dispatch(clock.now(), due, settings, statistics);
                connection.flush(statistics)
            });
            if let Err(kind) = result {
                deregister(connection, poller);
                connection.fail(kind, clock.now(), settings, statistics);
            }
        }
        State::Down { .. } => unreachable!("sockets are deregistered before they are closed"),
    }
}

/// Time out requests and connection attempts, and reconnect connections that still have work. Returns whether anything changed.
fn maintain<P: Poller>(
    load: &Load,
    connections: &mut [Connection<TcpStream>],
    poller: &mut P,
    now: u64,
    settings: &Settings,
    statistics: &mut WorkerStatistics,
) -> bool {
    let mut changed = false;
    for (local, connection) in connections.iter_mut().enumerate() {
        match connection.state {
            State::Open if connection.timed_out(now, settings) => {
                deregister(connection, poller);
                connection.fail(ErrorKind::Timeout, now, settings, statistics);
            }
            State::Connecting { since }
                if now.saturating_sub(since) >= settings.connect_timeout =>
            {
                deregister(connection, poller);
                connection.connect_failed(now, settings, statistics);
            }
            State::Down { until } if now >= until && connection.has_work(now, settings) => {
                let stream = os::connect(load.address).and_then(|stream| {
                    poller
                        .add(stream.as_raw_fd(), local as u64)
                        .map(|()| stream)
                });
                match stream {
                    Ok(stream) => connection.connecting(stream, now),
                    Err(_) => connection.connect_failed(now, settings, statistics),
                }
            }
            _ => continue,
        }
        changed = true;
    }
    changed
}

/// Whether the worker is done: every planned request has an outcome in the open loop, or the window has ended with nothing outstanding in the closed loop. At the drain deadline, whatever is left is abandoned.
fn done<P: Poller>(
    connections: &mut [Connection<TcpStream>],
    poller: &mut P,
    now: u64,
    planned: u64,
    settings: &Settings,
    statistics: &mut WorkerStatistics,
) -> bool {
    if now >= settings.drain_deadline {
        for connection in connections.iter_mut() {
            deregister(connection, poller);
            connection.abandon(now, settings, statistics);
        }
        return true;
    }
    match settings.schedule {
        Some(_) => statistics.finished() == planned,
        None => {
            now >= settings.timing.window_end
                && connections
                    .iter()
                    .all(|connection| connection.in_flight.is_empty())
        }
    }
}

#[cfg(test)]
mod tests;
