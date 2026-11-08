// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{
    clock::{Clock, MonotonicClock},
    histogram::DEFAULT_SIGNIFICANT_DIGITS,
    options::{BodyExpectation, Mode, Options, nanoseconds},
    os,
    poller::Epoll,
    report::{Report, WorkerReport, digest, merge},
    request::RequestTemplate,
    response::{Expectation, Response, ResponseParser},
    schedule::Schedule,
    worker::{Load, Start, work},
};
use base_benchmark::Interval;
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    time::Duration,
};

/// How far ahead of the moment every worker is connected the run starts, so that every worker sees the start before it arrives.
const START_LEAD: u64 = 100_000_000;
/// Descriptors needed beyond one per connection: standard streams, epoll instances, and the preflight connection.
const DESCRIPTOR_ALLOWANCE: u64 = 64;

/// Send one request on its own connection and validate the response, before any load, waiting at most `timeout` to connect and as long again between reads. Its body is captured, to become the expected body unless the options give one.
pub fn preflight(
    address: SocketAddr,
    request: &RequestTemplate,
    expectation: &Expectation,
    timeout: Duration,
) -> io::Result<Response> {
    TcpStream::connect_timeout(&address, timeout)
        .and_then(|stream| stream.set_read_timeout(Some(timeout)).map(|()| stream))
        .and_then(|mut stream| stream.write_all(&request.bytes).map(|()| stream))
        .and_then(|stream| read_response(stream, request, expectation))
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("preflight request to {address}: {error}"),
            )
        })
}

/// Read the preflight response. A read interrupted by a signal is retried: with a read timeout set, Linux never restarts it, whatever the signal's disposition.
fn read_response(
    mut stream: impl Read,
    request: &RequestTemplate,
    expectation: &Expectation,
) -> io::Result<Response> {
    let mut parser = ResponseParser::capturing();
    let mut buffer = Vec::new();
    let mut chunk = [0; 16 * 1024];
    loop {
        let read = match stream.read(&mut chunk) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            read => read?,
        };
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

/// Run the load the options describe, and report what it measured.
///
/// Before the load, one preflight request checks that the server answers with a valid response, within the connect timeout. Fails without measuring when the address does not resolve, the preflight fails, a worker cannot pin its CPU or connect, or the calling thread, which controls the run, cannot pin its CPU. Unpinned workers keep the calling thread's affinity from before the run.
pub fn run(options: &Options) -> io::Result<Report> {
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
    os::raise_descriptor_limit(options.connections as u64 + DESCRIPTOR_ALLOWANCE);
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
    let start = Start::new(load.workers);
    let (origin, sample, results) = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..load.workers)
            .map(|worker| {
                let (load, clock, start) = (&load, &clock, &start);
                std::thread::Builder::new()
                    .name(format!("load-worker-{worker}"))
                    .spawn_scoped(scope, move || {
                        work(load, worker, Epoll::new(), clock, start)
                    })
                    .expect("spawning a worker thread succeeds")
            })
            .collect();
        // Pin the controller only once every worker exists: a thread inherits its creator's affinity, so pinning first would confine every worker without a CPU of its own to the controller's CPU.
        let pinned = options.controller_cpu.map_or(Ok(()), os::pin);
        if let Some(error) = start.wait_for_workers().or(pinned.err()) {
            start.abort();
            return Err(error);
        }
        let origin = clock.now() + START_LEAD;
        start.publish(origin);
        sleep_until(&clock, origin.saturating_add(load.warmup));
        // The controller's interval approximates the window, which the workers time exactly: it starts and ends when this thread wakes, a little after each boundary.
        let interval = Interval::start().expect("measuring the current process succeeds");
        sleep_until(
            &clock,
            origin
                .saturating_add(load.warmup)
                .saturating_add(load.duration),
        );
        let sample = interval
            .finish(1)
            .expect("measuring the current process succeeds");
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("workers do not panic")
                    .expect("started workers return their results")
            })
            .collect();
        Ok((origin, sample, results))
    })?;
    let workers: Vec<WorkerReport> = results
        .into_iter()
        .enumerate()
        .map(|(index, result)| WorkerReport {
            cpu: options.cpus.get(index).copied(),
            affinity: result.affinity,
            statistics: result.statistics,
            connections: result.connections,
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
        dispatch_lag_limit: options.dispatch_lag_limit.map(nanoseconds),
        connection_details: options.connection_details,
        context: json!({
            "version": env!("CARGO_PKG_VERSION"),
            "debug_assertions": cfg!(debug_assertions),
            "clock_source": os::clock_source(),
            "address": address.to_string(),
            "cpus": options.cpus,
            "controller_cpu": options.controller_cpu,
        }),
    })
}

/// Everything that changes the work, so that `base-benchmark` can reject subjects that did different work. Placement and the address are context, not parameters.
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
            "length": request.bytes.len(),
            "digest": digest(&request.bytes),
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
