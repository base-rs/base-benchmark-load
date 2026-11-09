// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{
    clock::{Clock, MonotonicClock},
    histogram::DEFAULT_SIGNIFICANT_DIGITS,
    options::{BodyExpectation, Mode, Options, nanoseconds},
    os,
    poller::Epoll,
    report::{Markers, Report, WorkerReport, digest, merge, micros},
    request::RequestTemplate,
    response::{Expectation, Response, ResponseParser},
    schedule::Schedule,
    worker::{Load, Start, WorkerResult, work},
};
use base_benchmark::{Interval, Sample, Window};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    thread::Builder,
    time::Duration,
};

/// How far ahead of the moment every worker is connected the run starts, so that every worker sees the start before it arrives.
const START_LEAD: u64 = 100_000_000;
/// Descriptors needed beyond one per connection and one epoll instance per worker: the standard streams, the preflight connection, and whatever else the process holds, with room to spare.
const DESCRIPTOR_ALLOWANCE: u64 = 64;
/// The requests sent before the load, on a connection of their own: one preflight request. A server's own request count exceeds the record's outcomes by this many.
pub const PREFLIGHT_REQUESTS: u64 = 1;

/// Send one request on its own connection and validate the response, before any load, waiting at most `timeout` to connect and as long again between reads. Its body is captured, to become the expected body unless the options give one.
pub fn preflight(
    address: SocketAddr,
    request: &RequestTemplate,
    expectation: &Expectation,
    timeout: Duration,
) -> io::Result<Response> {
    TcpStream::connect_timeout(&address, timeout)
        .and_then(|mut stream| stream.write_all(&request.bytes).map(|()| stream))
        .and_then(|stream| {
            read_response(
                stream,
                nanoseconds(timeout),
                &MonotonicClock::new(),
                request,
                expectation,
            )
        })
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("preflight request to {address}: {error}"),
            )
        })
}

/// A stream whose reads wait at most a timeout.
trait TimedRead: Read {
    fn set_read_timeout(&self, timeout: Duration) -> io::Result<()>;
}

impl TimedRead for TcpStream {
    fn set_read_timeout(&self, timeout: Duration) -> io::Result<()> {
        TcpStream::set_read_timeout(self, Some(timeout))
    }
}

/// Read the preflight response, waiting at most `timeout` nanoseconds of `clock` for each read to return data. A read interrupted by a signal is retried, since with a read timeout set Linux never restarts it, whatever the signal's disposition; the retry waits only for what remains of the read's timeout, so frequent signals, such as a profiler's timer, cannot prolong the wait.
fn read_response(
    mut stream: impl TimedRead,
    timeout: u64,
    clock: &impl Clock,
    request: &RequestTemplate,
    expectation: &Expectation,
) -> io::Result<Response> {
    let mut parser = ResponseParser::capturing();
    let mut buffer = Vec::new();
    let mut chunk = [0; 16 * 1024];
    // When the current read times out; none until it starts.
    let mut deadline = None;
    loop {
        let now = clock.now();
        let remaining = deadline
            .get_or_insert(now.saturating_add(timeout))
            .saturating_sub(now);
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the server sent nothing more in time",
            ));
        }
        stream.set_read_timeout(Duration::from_nanos(remaining))?;
        let read = match stream.read(&mut chunk) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            read => read?,
        };
        deadline = None;
        if read == 0 {
            return parser.at_eof(expectation).ok_or(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the server closed the connection before a complete response",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        match parser.parse(&buffer, expectation, request.head) {
            Ok((_, Some(response))) => return Ok(response),
            Ok((consumed, None)) => drop(buffer.drain(..consumed)),
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the response could not be framed: {error:?}"),
                ));
            }
        }
    }
}

fn resolve(address: &str) -> io::Result<SocketAddr> {
    address
        .to_socket_addrs()
        .and_then(|mut addresses| {
            addresses.next().ok_or(io::Error::new(
                io::ErrorKind::NotFound,
                "the name has no addresses",
            ))
        })
        .map_err(|error| io::Error::new(error.kind(), format!("resolving {address}: {error}")))
}

fn sleep_until(clock: &MonotonicClock, instant: u64) {
    let now = clock.now();
    if instant > now {
        std::thread::sleep(Duration::from_nanos(instant - now));
    }
}

/// What the controller measured of a run.
#[derive(Debug)]
struct Controlled {
    /// The start.
    origin: u64,
    /// The controller's measurement of the window.
    sample: Sample,
    /// Each CPU's time in software interrupts over the controller's window, unless `/proc/stat` could not be read.
    softirq: Option<BTreeMap<usize, u64>>,
    /// When the window was marked, under the runner.
    markers: Option<Markers>,
    results: Vec<WorkerResult>,
}

/// The error of a window marker that could not be sent, or whose acknowledgement could not be read.
fn marking(which: &str, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("marking the window's {which} for base-benchmark: {error}"),
    )
}

/// Control a run from the calling thread: start `workers` threads, each running `worker`, which reports its setup to the [`Start`] it is given; once all have set up, pin the calling thread to `controller_cpu`, if any, and start them together; measure the window with an [`Interval`], and each CPU's software interrupt time over it; and collect their results. Each worker's thread is spawned from the builder `prepare` returns, given its index, a builder that names it, and the start.
///
/// With a `window`, under `base-benchmark`'s runner, the controller also marks the window, so that the runner's snapshots of the services bracket the counted requests. The runner acknowledges the start once it has taken its snapshots, and no request planned in the window, nor any outcome of the closed loop's window, comes before the window starts, so the start is marked, and its acknowledgement awaited, before the window does. When `lead` is at least the warmup, the start is marked once every worker has connected, before the start is published, which therefore follows the acknowledgement: the services' window then takes in the warmup, but its acknowledgement always comes first. Otherwise the start is published first, and the start is marked `lead` before the window, during the warmup, so that the services' window takes in at most `lead` of it; an acknowledgement that comes after the window has started fails the run. With `drained`, as in rate mode, whose window counts the requests planned in it, the stop is marked once every worker has finished, and so every request has an outcome, so that the services' window takes in their work for every request the window counts, however late it was sent or served; nothing is planned after the window, so the drain holds no other work but that of the warmup's requests still outstanding. Otherwise, as in the closed loop, whose window counts the outcomes that happen in it, the stop is marked when the controller wakes at the window's end, before the controller's interval ends.
///
/// Fails when a worker's thread cannot be spawned, as when the process may have no more threads or memory; when a worker fails to set up, including when it panics before it reports; or when the controller cannot pin its CPU. The workers that started are released and joined first, so a panic is reported as the error that names its worker. Fails too when a marker cannot be sent or its acknowledgement read, or comes too late: before the start is published, the workers are released without running, and after it, the run fails once they have finished.
#[allow(clippy::too_many_arguments)]
fn drive(
    workers: usize,
    controller_cpu: Option<usize>,
    warmup: u64,
    duration: u64,
    mut window: Option<&mut Window>,
    lead: u64,
    drained: bool,
    clock: &MonotonicClock,
    worker: impl Fn(usize, &Start) -> Option<WorkerResult> + Sync,
    prepare: impl Fn(usize, Builder, &Start) -> io::Result<Builder>,
) -> io::Result<Controlled> {
    let start = Start::new(workers);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for index in 0..workers {
            let (worker, start) = (&worker, &start);
            let spawned = prepare(
                index,
                Builder::new().name(format!("load-worker-{index}")),
                start,
            )
            .and_then(|builder| builder.spawn_scoped(scope, move || worker(index, start)));
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    // The workers that started wait for the others at the start, which they would never reach: abandon the run with them alone.
                    start.started_only(
                        index,
                        io::Error::new(
                            error.kind(),
                            format!(
                                "spawning worker {index} failed, with {index} of {workers} workers started: {error}"
                            ),
                        ),
                    );
                    break;
                }
            }
        }
        // Pin the controller only once every worker exists: a thread inherits its creator's affinity, so pinning first would confine every worker without a CPU of its own to the controller's CPU.
        let pinned = controller_cpu.map_or(Ok(()), os::pin);
        let mut failure = start.wait_for_workers().or(pinned.err());
        // Every worker has connected and waits for the start, so the services are idle: a start marked now, with the start published only once it is acknowledged, precedes every request.
        let before_start = lead >= warmup;
        let mut started = None;
        if let Some(window) = window
            .as_deref_mut()
            .filter(|_| before_start && failure.is_none())
        {
            let sent = clock.now();
            match window.start_acknowledged() {
                Ok(()) => started = Some((sent, clock.now())),
                Err(error) => failure = Some(marking("start", error)),
            }
        }
        if let Some(error) = failure {
            start.abort();
            // A worker that panicked is joined here, so that the run fails with the error naming it rather than the scope panicking.
            for handle in handles {
                let _ = handle.join();
            }
            return Err(error);
        }
        let origin = clock.now() + START_LEAD;
        start.publish(origin);
        let window_start = origin.saturating_add(warmup);
        let window_end = window_start.saturating_add(duration);
        // The workers now run to completion whatever happens here, so a failure is returned only once they have been joined.
        if let Some(window) = window.as_deref_mut().filter(|_| !before_start) {
            // The start is `START_LEAD` after the controller read the clock, and the lead is shorter than the warmup, so this is after now.
            sleep_until(clock, window_start - lead);
            let sent = clock.now();
            match window.start_acknowledged() {
                Ok(()) => started = Some((sent, clock.now())),
                Err(error) => failure = Some(marking("start", error)),
            }
        }
        if let Some((_, acknowledged)) =
            started.filter(|(_, acknowledged)| *acknowledged > window_start)
        {
            failure = Some(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "base-benchmark acknowledged the window's start {} after the window began, so its snapshots of the services may follow counted requests; lengthen --window-lead, now {}",
                    micros(acknowledged - window_start),
                    micros(lead)
                ),
            ));
        }
        sleep_until(clock, window_start);
        // The controller's interval approximates the window, which the workers time exactly: it starts and ends when this thread wakes, a little after each boundary.
        let interval = Interval::start().expect("measuring the current process succeeds");
        let softirq_start = os::softirq_times().ok();
        sleep_until(clock, window_end);
        // The stop is marked only for a window whose start was acknowledged in time.
        let mut stopping = window.zip(started).filter(|_| failure.is_none());
        let stop = |window: &mut Window, (start_sent, acknowledged): (u64, u64)| {
            let stop_sent = clock.now();
            window
                .stop()
                .map(|()| Markers {
                    lead,
                    before_start,
                    start_sent,
                    acknowledged,
                    stop_sent,
                    drained,
                })
                .map_err(|error| marking("stop", error))
        };
        let mut stopped = None;
        if !drained {
            stopped = stopping
                .take()
                .map(|(window, started)| stop(window, started));
        }
        let sample = interval
            .finish(1)
            .expect("measuring the current process succeeds");
        let softirq = softirq_start
            .zip(os::softirq_times().ok())
            .map(|(start, end)| {
                end.into_iter()
                    .map(|(cpu, time)| {
                        (
                            cpu,
                            time.saturating_sub(start.get(&cpu).copied().unwrap_or(time)),
                        )
                    })
                    .collect()
            });
        let results = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("workers do not panic")
                    .expect("started workers return their results")
            })
            .collect();
        // Every request now has an outcome, and its response, if any, has been read, so the services' work for every request planned in the window precedes this stop.
        if let Some((window, started)) = stopping {
            stopped = Some(stop(window, started));
        }
        match (failure, stopped.transpose()) {
            (Some(error), _) | (None, Err(error)) => Err(error),
            (None, Ok(markers)) => Ok(Controlled {
                origin,
                sample,
                softirq,
                markers,
                results,
            }),
        }
    })
}

/// Run the load the options describe, and report what it measured, without marking its window: as a driver outside `base-benchmark`'s runner.
///
/// Before the load, one preflight request checks that the server answers with a valid response, within the connect timeout. Fails without measuring when the address does not resolve, the preflight fails, a worker's thread cannot be spawned, a worker cannot pin its CPU or connect, or panics while setting up, or the calling thread, which controls the run, cannot pin its CPU. Unpinned workers keep the calling thread's affinity from before the run.
pub fn run(options: &Options) -> io::Result<Report> {
    run_with_window(options, None)
}

/// Run the load the options describe, as [`run`] does, and mark its measured window with `window`, as a driver under `base-benchmark`'s runner, which gives it with [`Window::from_environment`]. The calling thread, which controls the run, sends the markers, so the window's output, such as stdout, must have nothing else written to it during the run.
///
/// The start marker is sent before the window starts, `--window-lead` before it, or once every worker has connected when the lead is at least the warmup, and the run waits for its acknowledgement, so the runner's snapshots of the services precede the window: in rate mode, every request planned in it, and in the closed loop, every outcome it counts. A request the closed loop counts was sent before its outcome, and reached the server before the snapshots only if it was outstanding for longer than the time from the acknowledgement to the window's start, about the lead; with the start marked before the load, none did. In rate mode the stop marker is sent once every request has an outcome, after the drain, so that the services' work for requests planned in the window but sent or served after it, as when the client lags or the server falls behind, is in the services' window too; in the closed loop, whose window counts the outcomes in it, when the window ends. Fails, besides as [`run`] does, when a marker cannot be sent, the acknowledgement cannot be read or is not one, or it comes after the window has started; after the load has started, only once every request has an outcome.
pub fn run_with_window(options: &Options, window: Option<&mut Window>) -> io::Result<Report> {
    let address = resolve(&options.address)?;
    let request = RequestTemplate::new(
        &options.method,
        &options.path,
        &options.address,
        &options.headers,
        &options.request_body,
    );
    let mut expectation = Expectation {
        status: options.status,
        headers: options
            .expect_headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.as_bytes().to_vec()))
            .collect(),
        body: match &options.body {
            BodyExpectation::Exact(body) => Some(body.clone()),
            BodyExpectation::Preflight | BodyExpectation::Any => None,
        },
        response: options.response.clone(),
    };
    let preflight = preflight(address, &request, &expectation, options.connect_timeout)?;
    if let Some(kind) = preflight.invalid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the preflight response is invalid ({}): status {}",
                kind.name(),
                preflight.status
            ),
        ));
    }
    if options.body == BodyExpectation::Preflight && options.response.is_none() {
        expectation.body = preflight.body;
    }
    let descriptor_limit = os::raise_descriptor_limit(
        options.connections as u64 + options.workers as u64 + DESCRIPTOR_ALLOWANCE,
    );
    let load = Load {
        address,
        connections: options.connections,
        workers: options.workers,
        cpus: &options.cpus,
        depth: options.depth,
        schedule: options.rate.map(Schedule::new),
        warmup: nanoseconds(options.warmup),
        duration: nanoseconds(options.duration),
        timeout: nanoseconds(options.timeout),
        connect_timeout: nanoseconds(options.connect_timeout),
        drain_timeout: nanoseconds(options.drain_timeout),
        reconnect_delay: nanoseconds(options.reconnect_delay),
        interval: nanoseconds(options.interval),
        request: &request,
        expectation: &expectation,
    };
    let clock = MonotonicClock::new();
    let Controlled {
        origin,
        sample,
        softirq,
        markers,
        results,
    } = drive(
        load.workers,
        options.controller_cpu,
        load.warmup,
        load.duration,
        window,
        nanoseconds(options.window_lead),
        options.mode == Mode::Rate,
        &clock,
        |worker, start| work(&load, worker, Epoll::new(), &clock, start),
        |_, builder, _| Ok(builder),
    )?;
    let workers: Vec<WorkerReport> = results
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            let cpu = options.cpus.get(index).copied();
            WorkerReport {
                cpu,
                affinity: result.affinity,
                statistics: result.statistics,
                connections: result.connections,
                softirq: cpu
                    .and_then(|cpu| softirq.as_ref().and_then(|times| times.get(&cpu).copied())),
            }
        })
        .collect();
    let settings = load.settings(origin);
    Ok(Report {
        mode: options.mode,
        parameters: parameters(options, &request, &expectation),
        signature: signature(&expectation, preflight.status),
        sample,
        total: merge(&workers),
        workers,
        planned: settings.total - settings.measured_first,
        window: load.duration,
        window_end: settings.timing.window_end,
        interval: load.interval,
        busy_limit: options.busy_limit,
        schedule_lag_limit: nanoseconds(options.schedule_lag_limit),
        shortfall_limit: options.shortfall_limit,
        starved_limit: options.starved_limit,
        connection_details: options.connection_details,
        preflight_requests: PREFLIGHT_REQUESTS,
        markers,
        context: json!({
            "version": env!("CARGO_PKG_VERSION"),
            "debug_assertions": cfg!(debug_assertions),
            "clock_source": os::clock_source(),
            "address": address.to_string(),
            "cpus": options.cpus,
            "controller_cpu": options.controller_cpu,
            "descriptor_limit": descriptor_limit,
        }),
    })
}

/// Everything that changes the work, so that `base-benchmark` can reject subjects that did different work, and nothing that varies between equal runs. Placement and the address are context, not parameters, so the request is identified without the `Host` header added by default, which names the address.
fn parameters(options: &Options, request: &RequestTemplate, expectation: &Expectation) -> Value {
    let body = match &options.body {
        _ if options.response.is_some() => json!("response"),
        BodyExpectation::Exact(body) => json!({"length": body.len(), "digest": digest(body)}),
        BodyExpectation::Preflight => json!("preflight"),
        BodyExpectation::Any => json!("any"),
    };
    let headers: Vec<Value> = options
        .expect_headers
        .iter()
        .map(|(name, value)| json!([name.to_ascii_lowercase(), value]))
        .collect();
    json!({
        "mode": options.mode.name(),
        "rate": options.rate.map(|rate| rate.get()),
        "arrivals": (options.mode == Mode::Rate).then_some("uniform"),
        "assignment": (options.mode == Mode::Rate).then_some("per-connection"),
        "connections": options.connections,
        "depth": options.depth,
        "workers": options.workers,
        "warmup_ns": nanoseconds(options.warmup),
        "duration_ns": nanoseconds(options.duration),
        "timeout_ns": nanoseconds(options.timeout),
        "connect_timeout_ns": nanoseconds(options.connect_timeout),
        "drain_timeout_ns": nanoseconds(options.drain_timeout),
        "reconnect_delay_ns": nanoseconds(options.reconnect_delay),
        "interval_ns": nanoseconds(options.interval),
        "request": {
            "method": options.method,
            "path": options.path,
            "host": if request.default_host { "default" } else { "header" },
            "length": request.identity.len(),
            "digest": digest(&request.identity),
        },
        "validation": {
            "status": expectation.status,
            "body": body,
            "headers": headers,
            "response": expectation.response.as_deref().map(digest),
        },
        "histogram_significant_digits": DEFAULT_SIGNIFICANT_DIGITS,
    })
}

/// The identity of a valid response: its status and body digest, or the digest of the whole response for raw servers. Headers are left out, since their case and order legitimately differ between servers.
fn signature(expectation: &Expectation, status: u16) -> String {
    match (&expectation.response, &expectation.body) {
        (Some(response), _) => format!("http1 response {}:{}", response.len(), digest(response)),
        (None, Some(body)) => format!("http1 status {status} body {}:{}", body.len(), digest(body)),
        (None, None) => format!("http1 status {status} body any"),
    }
}

#[cfg(test)]
mod tests;
