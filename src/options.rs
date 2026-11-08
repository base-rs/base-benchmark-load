// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::{io, num::NonZeroU64, time::Duration};

/// The usage text, printed by `help`.
pub const USAGE: &str = "\
Usage: base-benchmark-load MODE --address HOST:PORT [OPTIONS]

Modes:
  closed                     Keep every connection busy and measure service time under saturation.
  rate                       Send at a fixed rate on an absolute schedule and measure latency from each request's intended send time.

Load:
  --address HOST:PORT        The server (required).
  --connections N            Persistent connections [64].
  --depth N                  Outstanding (pipelined) requests per connection [1].
  --rate N                   Offered requests per second in rate mode, such as 240000 or 240k (required there).
  --cpus LIST                Pin one worker to each CPU, such as 2,3,6-7.
  --workers N                Unpinned workers, when --cpus is not given [1].
  --controller-cpu N         Pin the controlling thread.
  --warmup DURATION          Load before the measured window [1s]. Durations take ns, us, ms, s, or m; a bare number is seconds.
  --duration DURATION        The measured window [5s].
  --timeout DURATION         The longest wait for a response after its send [10s].
  --connect-timeout DURATION The longest wait for a connection, and for the preflight response [10s].
  --drain-timeout DURATION   How long requests still outstanding after the window may take [15s].
  --reconnect-delay DURATION The pause before reconnecting a failed connection [1ms].
  --interval DURATION        The length of the reported intervals within the window [1s].

Request:
  --method METHOD            [GET]
  --path PATH                The request target [/].
  --header 'NAME: VALUE'     A request header; repeatable. Host defaults to the address.
  --request-body TEXT        A request body, sent with Content-Length.

Validation (of every response):
  --status CODE              The expected status [200].
  --body TEXT                The exact expected body. Without it, the body of a preflight response is expected.
  --body-file PATH           The exact expected body, from a file.
  --any-body                 Accept any correctly framed body.
  --expect-header 'NAME: VALUE'  A required response header; repeatable. Names match case-insensitively.
  --response-file PATH       The exact expected bytes of every whole response, for raw servers.

Output:
  --connection-details       Include every connection's counters in the record.
  --busy-limit FRACTION      Flag the client as saturated when a worker is busy for more of the window [0.9].
  --dispatch-lag-limit DURATION  Flag the client as saturated when the 99th percentile dispatch lag exceeds this.
  --fail-on CLASSES          Exit unsuccessfully, without a record, on invalid responses, errors, or saturation: a comma-separated list of invalid, errors, saturated.
  --quiet                    Do not write the summary to stderr.

The record goes to stdout as one line of JSON. Other commands: help, version.
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every connection keeps `depth` requests outstanding, sending the next as soon as one completes.
    Closed,
    /// Requests follow an absolute schedule at the offered rate, whatever the responses do.
    Rate,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Rate => "rate",
        }
    }
}

/// Which response body is valid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyExpectation {
    /// The body of the preflight response, which every response must then repeat.
    Preflight,
    Exact(Vec<u8>),
    Any,
}

/// The outcomes that make the command exit unsuccessfully.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FailOn {
    pub invalid: bool,
    pub errors: bool,
    pub saturated: bool,
}

/// A parsed command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub mode: Mode,
    pub address: String,
    pub connections: usize,
    pub depth: usize,
    pub rate: Option<NonZeroU64>,
    pub cpus: Vec<usize>,
    pub workers: usize,
    pub controller_cpu: Option<usize>,
    pub warmup: Duration,
    pub duration: Duration,
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub drain_timeout: Duration,
    pub reconnect_delay: Duration,
    pub interval: Duration,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub request_body: Vec<u8>,
    pub status: u16,
    pub body: BodyExpectation,
    pub expect_headers: Vec<(String, String)>,
    pub response: Option<Vec<u8>>,
    pub connection_details: bool,
    pub busy_limit: f64,
    pub dispatch_lag_limit: Option<Duration>,
    pub fail_on: FailOn,
    pub quiet: bool,
}

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Run(Box<Options>),
    Help,
    Version,
}

/// The options that take a value.
const VALUE_OPTIONS: [&str; 26] = [
    "--address",
    "--connections",
    "--depth",
    "--rate",
    "--cpus",
    "--workers",
    "--controller-cpu",
    "--warmup",
    "--duration",
    "--timeout",
    "--connect-timeout",
    "--drain-timeout",
    "--reconnect-delay",
    "--interval",
    "--method",
    "--path",
    "--header",
    "--request-body",
    "--status",
    "--body",
    "--body-file",
    "--expect-header",
    "--response-file",
    "--busy-limit",
    "--dispatch-lag-limit",
    "--fail-on",
];

/// The most reporting intervals a window may have; each needs a small histogram per worker.
pub const INTERVAL_COUNT_LIMIT: u64 = 100_000;

impl Options {
    /// Options for `mode` with every default, and no address.
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            address: String::new(),
            connections: 64,
            depth: 1,
            rate: None,
            cpus: Vec::new(),
            workers: 1,
            controller_cpu: None,
            warmup: Duration::from_secs(1),
            duration: Duration::from_secs(5),
            timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
            drain_timeout: Duration::from_secs(15),
            reconnect_delay: Duration::from_millis(1),
            interval: Duration::from_secs(1),
            method: "GET".to_owned(),
            path: "/".to_owned(),
            headers: Vec::new(),
            request_body: Vec::new(),
            status: 200,
            body: BodyExpectation::Preflight,
            expect_headers: Vec::new(),
            response: None,
            connection_details: false,
            busy_limit: 0.9,
            dispatch_lag_limit: None,
            fail_on: FailOn::default(),
            quiet: false,
        }
    }

    /// Parse a command line, without the program name.
    pub fn parse(arguments: impl IntoIterator<Item = String>) -> io::Result<Command> {
        let mut arguments = arguments.into_iter();
        let mut options = match arguments.next().as_deref() {
            Some("closed") => Self::new(Mode::Closed),
            Some("rate") => Self::new(Mode::Rate),
            Some("help" | "--help" | "-h") => return Ok(Command::Help),
            Some("version" | "--version") => return Ok(Command::Version),
            Some(other) => {
                return Err(invalid(format!(
                    "unknown mode {other:?}; expected closed or rate"
                )));
            }
            None => return Err(invalid("a mode is required: closed or rate")),
        };
        let mut workers = None;
        let mut body = None;
        while let Some(argument) = arguments.next() {
            let (name, inline) = match argument.split_once('=') {
                Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
                None => (argument, None),
            };
            match name.as_str() {
                "--help" => return Ok(Command::Help),
                "--any-body" | "--connection-details" | "--quiet" => {
                    if inline.is_some() {
                        return Err(invalid(format!("{name} takes no value")));
                    }
                    match name.as_str() {
                        "--any-body" => body = Some(BodyExpectation::Any),
                        "--connection-details" => options.connection_details = true,
                        _ => options.quiet = true,
                    }
                    continue;
                }
                _ if !VALUE_OPTIONS.contains(&name.as_str()) => {
                    return Err(invalid(format!("unknown option {name:?}")));
                }
                _ => {}
            }
            let value = inline
                .or_else(|| arguments.next())
                .ok_or_else(|| invalid(format!("{name} needs a value")))?;
            match name.as_str() {
                "--address" => {
                    options.address = value;
                    Ok(())
                }
                "--connections" => count(&name, &value).map(|count| options.connections = count),
                "--depth" => count(&name, &value).map(|count| options.depth = count),
                "--rate" => rate(&value).map(|rate| options.rate = Some(rate)),
                "--cpus" => cpus(&value).map(|cpus| options.cpus = cpus),
                "--workers" => count(&name, &value).map(|count| workers = Some(count)),
                "--controller-cpu" => cpu(&value).map(|cpu| options.controller_cpu = Some(cpu)),
                "--warmup" => duration(&value).map(|duration| options.warmup = duration),
                "--duration" => duration(&value).map(|duration| options.duration = duration),
                "--timeout" => duration(&value).map(|duration| options.timeout = duration),
                "--connect-timeout" => {
                    duration(&value).map(|duration| options.connect_timeout = duration)
                }
                "--drain-timeout" => {
                    duration(&value).map(|duration| options.drain_timeout = duration)
                }
                "--reconnect-delay" => {
                    duration(&value).map(|duration| options.reconnect_delay = duration)
                }
                "--interval" => duration(&value).map(|duration| options.interval = duration),
                "--method" => token(&name, value).map(|method| options.method = method),
                "--path" => line(&name, value).map(|path| options.path = path),
                "--header" => header(&value).map(|header| options.headers.push(header)),
                "--request-body" => {
                    options.request_body = value.into_bytes();
                    Ok(())
                }
                "--status" => status(&value).map(|status| options.status = status),
                "--body" => {
                    body = Some(BodyExpectation::Exact(value.into_bytes()));
                    Ok(())
                }
                "--body-file" => {
                    read(&value).map(|bytes| body = Some(BodyExpectation::Exact(bytes)))
                }
                "--expect-header" => {
                    header(&value).map(|header| options.expect_headers.push(header))
                }
                "--response-file" => read(&value).map(|bytes| options.response = Some(bytes)),
                "--busy-limit" => fraction(&value).map(|fraction| options.busy_limit = fraction),
                "--dispatch-lag-limit" => {
                    duration(&value).map(|limit| options.dispatch_lag_limit = Some(limit))
                }
                _ => fail_on(options.fail_on, &value).map(|fail_on| options.fail_on = fail_on),
            }?;
        }
        if let Some(body) = body {
            options.body = body;
        }
        options.workers = match (workers, options.cpus.len()) {
            (Some(workers), 0) => workers,
            (None, 0) => 1,
            (Some(workers), cpus) if workers != cpus => {
                return Err(invalid(format!(
                    "--workers {workers} does not match the {cpus} CPUs of --cpus"
                )));
            }
            (_, cpus) => cpus,
        };
        options.validate()?;
        Ok(Command::Run(Box::new(options)))
    }

    /// Check combinations that each option alone cannot.
    pub fn validate(&self) -> io::Result<()> {
        if self.address.is_empty() {
            return Err(invalid("--address is required"));
        }
        match (self.mode, self.rate) {
            (Mode::Rate, None) => return Err(invalid("rate mode needs --rate")),
            (Mode::Closed, Some(_)) => return Err(invalid("--rate applies only to rate mode")),
            _ => {}
        }
        if self.connections < self.workers {
            return Err(invalid(format!(
                "{} connections cannot be shared by {} workers",
                self.connections, self.workers
            )));
        }
        if self.duration.is_zero()
            || self.interval.is_zero()
            || self.timeout.is_zero()
            || self.connect_timeout.is_zero()
        {
            return Err(invalid(
                "--duration, --interval, --timeout, and --connect-timeout must be positive",
            ));
        }
        let intervals = self.duration.as_nanos().div_ceil(self.interval.as_nanos());
        if intervals > u128::from(INTERVAL_COUNT_LIMIT) {
            return Err(invalid(format!(
                "the window has {intervals} intervals, more than {INTERVAL_COUNT_LIMIT}; lengthen --interval"
            )));
        }
        if let Some(rate) = self.rate {
            let schedule = crate::schedule::Schedule::new(rate);
            let warmup = nanoseconds(self.warmup);
            if schedule.first_at_or_after(warmup + nanoseconds(self.duration))
                == schedule.first_at_or_after(warmup)
            {
                return Err(invalid(
                    "no request is planned in the window; raise --rate or lengthen --duration",
                ));
            }
        }
        if self.response.is_some()
            && (self.body != BodyExpectation::Preflight || !self.expect_headers.is_empty())
        {
            return Err(invalid(
                "--response-file replaces --body, --body-file, --any-body, and --expect-header",
            ));
        }
        Ok(())
    }
}

/// A duration in nanoseconds, saturating at about 584 years.
pub fn nanoseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn count(name: &str, value: &str) -> io::Result<usize> {
    match value.parse() {
        Ok(count) if count > 0 => Ok(count),
        _ => Err(invalid(format!(
            "{name} needs a positive integer, not {value:?}"
        ))),
    }
}

fn cpu(value: &str) -> io::Result<usize> {
    match value.parse() {
        Ok(cpu) if cpu < libc::CPU_SETSIZE as usize => Ok(cpu),
        _ => Err(invalid(format!(
            "{value:?} is not a CPU number below {}",
            libc::CPU_SETSIZE
        ))),
    }
}

/// A CPU list such as `2,3,6-7`.
fn cpus(value: &str) -> io::Result<Vec<usize>> {
    let mut cpus = Vec::new();
    for part in value.split(',') {
        let (first, last) = part.split_once('-').unwrap_or((part, part));
        let (first, last) = cpu(first).and_then(|first| Ok((first, cpu(last)?)))?;
        if first > last {
            return Err(invalid(format!("{part:?} is not an increasing CPU range")));
        }
        cpus.extend(first..=last);
    }
    let mut unique = cpus.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != cpus.len() {
        return Err(invalid(format!("{value:?} names a CPU twice")));
    }
    Ok(cpus)
}

/// A decimal number with an optional fraction, scaled by `scale` and required to be a whole number afterwards.
fn scaled(value: &str, scale: u128) -> Option<u128> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() && fraction.is_empty()
        || !digits(whole)
        || !digits(fraction)
        || fraction.len() > 9
    {
        return None;
    }
    let denominator = 10u128.pow(fraction.len() as u32);
    let number: u128 = format!("{whole}{fraction}").parse().ok()?;
    let product = number.checked_mul(scale)?;
    (product % denominator == 0).then_some(product / denominator)
}

/// A duration such as `1.5s`, `250ms`, `100us`, or `2`, which is seconds.
pub fn duration(value: &str) -> io::Result<Duration> {
    let split = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let scale = match unit {
        "ns" => 1,
        "us" | "µs" => 1_000,
        "ms" => 1_000_000,
        "s" | "" => 1_000_000_000,
        "m" => 60_000_000_000,
        _ => 0,
    };
    scaled(number, scale)
        .filter(|_| scale > 0)
        .and_then(|nanoseconds| u64::try_from(nanoseconds).ok())
        .map(Duration::from_nanos)
        .ok_or_else(|| {
            invalid(format!(
                "{value:?} is not a duration such as 1.5s, 250ms, or 100us"
            ))
        })
}

/// A rate such as `240000`, `240k`, or `1.5m` requests per second.
fn rate(value: &str) -> io::Result<NonZeroU64> {
    let (number, scale) = match value.strip_suffix(&['k', 'K'][..]) {
        Some(number) => (number, 1_000),
        None => match value.strip_suffix(&['m', 'M'][..]) {
            Some(number) => (number, 1_000_000),
            None => (value, 1),
        },
    };
    scaled(number, scale)
        .and_then(|rate| u64::try_from(rate).ok())
        .and_then(NonZeroU64::new)
        .ok_or_else(|| {
            invalid(format!(
                "{value:?} is not a positive whole number of requests per second"
            ))
        })
}

fn fraction(value: &str) -> io::Result<f64> {
    match value.parse::<f64>() {
        Ok(fraction) if (0.0..=1.0).contains(&fraction) => Ok(fraction),
        _ => Err(invalid(format!("{value:?} is not a fraction from 0 to 1"))),
    }
}

fn status(value: &str) -> io::Result<u16> {
    match value.parse() {
        Ok(status) if (200..=999).contains(&status) => Ok(status),
        _ => Err(invalid(format!("{value:?} is not a final status code"))),
    }
}

fn fail_on(mut fail_on: FailOn, value: &str) -> io::Result<FailOn> {
    for class in value.split(',') {
        match class {
            "invalid" => fail_on.invalid = true,
            "errors" => fail_on.errors = true,
            "saturated" => fail_on.saturated = true,
            _ => {
                return Err(invalid(format!(
                    "unknown --fail-on class {class:?}; expected invalid, errors, or saturated"
                )));
            }
        }
    }
    Ok(fail_on)
}

/// A method: an HTTP token.
fn token(name: &str, value: String) -> io::Result<String> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte));
    if valid {
        Ok(value)
    } else {
        Err(invalid(format!("{name} {value:?} is not an HTTP token")))
    }
}

/// A value without control characters, which could otherwise end the request line or a header.
fn line(name: &str, value: String) -> io::Result<String> {
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(invalid(format!(
            "{name} {value:?} is empty or contains control characters"
        )));
    }
    Ok(value)
}

/// A header such as `Accept: */*`.
fn header(value: &str) -> io::Result<(String, String)> {
    let (name, field) = value
        .split_once(':')
        .ok_or_else(|| invalid(format!("{value:?} is not a header such as 'Name: value'")))?;
    let name = token("a header name", name.to_owned())?;
    let field = field.trim();
    if field
        .bytes()
        .any(|byte| byte.is_ascii_control() && byte != b'\t')
    {
        return Err(invalid(format!(
            "the header value {field:?} contains control characters"
        )));
    }
    Ok((name, field.to_owned()))
}

fn read(path: &str) -> io::Result<Vec<u8>> {
    std::fs::read(path)
        .map_err(|error| io::Error::new(error.kind(), format!("reading {path}: {error}")))
}

#[cfg(test)]
mod tests;
