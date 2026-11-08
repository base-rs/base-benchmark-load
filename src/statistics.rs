// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::histogram::{DEFAULT_SIGNIFICANT_DIGITS, Histogram};

/// The precision of the per-interval latency histograms, which only report a few quantiles each.
pub const INTERVAL_SIGNIFICANT_DIGITS: u8 = 2;

/// Why a request failed without a complete response. Each failed request is counted once, by the failure that ended it; every request outstanding on a connection that fails is charged with that connection's failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Writing the request failed, other than by a reset.
    Write,
    /// Reading the response failed, other than by a reset.
    Read,
    /// The connection was reset or the peer stopped reading (`ECONNRESET`, `EPIPE`, `ECONNABORTED`).
    Reset,
    /// The server closed the connection before the response was complete.
    Eof,
    /// No complete response arrived within the timeout of its send, or before the drain deadline.
    Timeout,
    /// The response could not be framed: a malformed head, an oversized head, invalid lengths or chunks, an unsupported protocol switch, or bytes that no request asked for.
    Parse,
    /// An earlier response on the connection closed it, ending the connection before this request's response: it said `Connection: close`, or its body was delimited by the close.
    Closed,
    /// The request was planned but never sent before the drain deadline (open loop only).
    Unsent,
}

impl ErrorKind {
    pub const ALL: [Self; 8] = [
        Self::Write,
        Self::Read,
        Self::Reset,
        Self::Eof,
        Self::Timeout,
        Self::Parse,
        Self::Closed,
        Self::Unsent,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Read => "read",
            Self::Reset => "reset",
            Self::Eof => "eof",
            Self::Timeout => "timeout",
            Self::Parse => "parse",
            Self::Closed => "closed",
            Self::Unsent => "unsent",
        }
    }
}

/// Why a complete, correctly framed response was invalid. Each response is counted once, by the first check it fails, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidKind {
    /// The status code differs from the expected one.
    Status,
    /// A required header is missing or has another value.
    Header,
    /// The body differs from the expected body.
    Body,
    /// The complete response bytes differ from the expected response.
    Response,
}

impl InvalidKind {
    pub const ALL: [Self; 4] = [Self::Status, Self::Header, Self::Body, Self::Response];

    pub fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Header => "header",
            Self::Body => "body",
            Self::Response => "response",
        }
    }
}

/// The outcomes of the requests of one period: valid and invalid responses, and failed requests by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcomes {
    pub valid: u64,
    pub invalid: [u64; InvalidKind::ALL.len()],
    pub errors: [u64; ErrorKind::ALL.len()],
}

impl Outcomes {
    pub fn invalid_total(&self) -> u64 {
        self.invalid.iter().sum()
    }

    pub fn error_total(&self) -> u64 {
        self.errors.iter().sum()
    }

    /// Every request with an outcome.
    pub fn total(&self) -> u64 {
        self.valid + self.invalid_total() + self.error_total()
    }

    pub fn merge(&mut self, other: &Self) {
        self.valid += other.valid;
        for (count, other) in self.invalid.iter_mut().zip(other.invalid) {
            *count += other;
        }
        for (count, other) in self.errors.iter_mut().zip(other.errors) {
            *count += other;
        }
    }
}

/// The period a request's outcome belongs to.
///
/// In the open loop, a request belongs to the period its planned send time falls in, so the measured cohort is every request planned in the window, however late it completes. In the closed loop, a request belongs to the period its outcome falls in: completions after the window are the drain of requests still in flight when it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Warmup,
    Measured,
    Drain,
}

impl Period {
    pub const ALL: [Self; 3] = [Self::Warmup, Self::Measured, Self::Drain];

    pub fn name(self) -> &'static str {
        match self {
            Self::Warmup => "warmup",
            Self::Measured => "measured",
            Self::Drain => "drain",
        }
    }
}

/// When a run's periods begin and end, in nanoseconds of the run's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Whether the run is closed-loop, which decides how outcomes are assigned to periods.
    pub closed: bool,
    /// The instant every worker starts sending, and the origin of the open-loop schedule.
    pub start: u64,
    pub window_start: u64,
    pub window_end: u64,
    /// The length of the reporting intervals within the window.
    pub interval: u64,
}

impl Timing {
    fn period(&self, request: &Request, now: u64) -> Period {
        let instant = if self.closed { now } else { request.planned };
        if instant < self.window_start {
            Period::Warmup
        } else if instant < self.window_end {
            Period::Measured
        } else {
            Period::Drain
        }
    }

    fn in_window(&self, instant: u64) -> bool {
        (self.window_start..self.window_end).contains(&instant)
    }

    /// The reporting interval holding `instant`, which is in the window.
    fn interval_index(&self, instant: u64) -> usize {
        ((instant - self.window_start) / self.interval) as usize
    }

    /// How many reporting intervals cover the window; the last may be partial.
    pub fn interval_count(&self) -> usize {
        (self.window_end - self.window_start).div_ceil(self.interval) as usize
    }
}

/// A request that has been sent, with its timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// Its position in the open-loop schedule; unused in the closed loop.
    pub ordinal: u64,
    /// When it should have been sent: its scheduled time in the open loop, and the start in the closed loop.
    pub planned: u64,
    /// When it was handed to the connection to send.
    pub sent: u64,
    /// When it could first have been sent: the later of its planned time and the moment its connection last had room for another outstanding request.
    pub eligible: u64,
}

/// One connection's counters, for fairness across connections. Completions and latencies cover the measured period only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectionCounters {
    pub valid: u64,
    pub invalid: u64,
    pub errors: u64,
    pub latency_sum: u128,
    pub latency_max: u64,
    pub bytes_read: u64,
}

/// One reporting interval of the window: completions in it, and the latency of the measured requests assigned to it, by planned time in the open loop and by completion in the closed loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntervalStatistics {
    pub completions: u64,
    pub latency: Histogram,
}

/// The distributions of the measured period's valid responses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Distributions {
    /// Completion minus planned send time in the open loop; unused in the closed loop.
    pub latency: Histogram,
    /// Completion minus actual send: the service time in the closed loop.
    pub round_trip: Histogram,
    /// Actual minus planned send time (open loop).
    pub send_delay: Histogram,
    /// Actual send minus the time the request became eligible to send: the client's own delay.
    pub dispatch_lag: Histogram,
}

impl Default for Distributions {
    fn default() -> Self {
        let histogram = || Histogram::new(DEFAULT_SIGNIFICANT_DIGITS);
        Self {
            latency: histogram(),
            round_trip: histogram(),
            send_delay: histogram(),
            dispatch_lag: histogram(),
        }
    }
}

impl Distributions {
    pub fn merge(&mut self, other: &Self) {
        self.latency.merge(&other.latency);
        self.round_trip.merge(&other.round_trip);
        self.send_delay.merge(&other.send_delay);
        self.dispatch_lag.merge(&other.dispatch_lag);
    }
}

/// Everything one worker measured. Workers share nothing while running; their statistics merge exactly afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerStatistics {
    /// Outcomes by [`Period`].
    pub periods: [Outcomes; 3],
    pub distributions: Distributions,
    pub intervals: Vec<IntervalStatistics>,
    pub sent: u64,
    pub sent_in_window: u64,
    pub completed_in_window: u64,
    /// The latest completion or failure of a request, if any.
    pub last_outcome: Option<u64>,
    /// Connections established, including reconnections.
    pub connects: u64,
    /// Connection attempts that failed or timed out. These are attempts, not requests.
    pub connect_errors: u64,
    /// Connections closed after a failure or by the server.
    pub disconnects: u64,
    /// Connections the server closed while no request was outstanding.
    pub idle_closes: u64,
    /// Writes that found the socket's send buffer full.
    pub write_would_block: u64,
    pub bytes_written: u64,
    pub bytes_read: u64,
    /// Event loop iterations that started in the window.
    pub iterations: u64,
    /// Time in the window spent in iterations that did work: handled events, sent requests, or maintained connections.
    pub busy: u64,
    /// The worker thread's CPU time over the window.
    pub cpu: u64,
}

impl WorkerStatistics {
    pub fn new(timing: &Timing) -> Self {
        let intervals = (0..timing.interval_count())
            .map(|_| IntervalStatistics {
                completions: 0,
                latency: Histogram::new(INTERVAL_SIGNIFICANT_DIGITS),
            })
            .collect();
        Self {
            periods: [Outcomes::default(); 3],
            distributions: Distributions::default(),
            intervals,
            sent: 0,
            sent_in_window: 0,
            completed_in_window: 0,
            last_outcome: None,
            connects: 0,
            connect_errors: 0,
            disconnects: 0,
            idle_closes: 0,
            write_would_block: 0,
            bytes_written: 0,
            bytes_read: 0,
            iterations: 0,
            busy: 0,
            cpu: 0,
        }
    }

    pub fn period(&self, period: Period) -> &Outcomes {
        &self.periods[period as usize]
    }

    /// Every request with an outcome, in every period.
    pub fn finished(&self) -> u64 {
        self.periods.iter().map(Outcomes::total).sum()
    }

    pub fn sent(&mut self, timing: &Timing, now: u64) {
        self.sent += 1;
        if timing.in_window(now) {
            self.sent_in_window += 1;
        }
    }

    /// Record a complete response to `request` at `now`; `invalid` says why it was invalid, if it was.
    pub fn complete(
        &mut self,
        timing: &Timing,
        request: &Request,
        now: u64,
        invalid: Option<InvalidKind>,
        connection: &mut ConnectionCounters,
    ) {
        self.last_outcome = self.last_outcome.max(Some(now));
        if timing.in_window(now) {
            self.completed_in_window += 1;
            self.intervals[timing.interval_index(now)].completions += 1;
        }
        let period = timing.period(request, now);
        let outcomes = &mut self.periods[period as usize];
        if let Some(kind) = invalid {
            outcomes.invalid[kind as usize] += 1;
            if period == Period::Measured {
                connection.invalid += 1;
            }
            return;
        }
        outcomes.valid += 1;
        if period != Period::Measured {
            return;
        }
        let round_trip = now.saturating_sub(request.sent);
        let distributions = &mut self.distributions;
        distributions.round_trip.record(round_trip);
        distributions
            .dispatch_lag
            .record(request.sent.saturating_sub(request.eligible));
        let (latency, interval) = if timing.closed {
            (round_trip, timing.interval_index(now))
        } else {
            let latency = now.saturating_sub(request.planned);
            distributions.latency.record(latency);
            distributions
                .send_delay
                .record(request.sent.saturating_sub(request.planned));
            (latency, timing.interval_index(request.planned))
        };
        self.intervals[interval].latency.record(latency);
        connection.valid += 1;
        connection.latency_sum += u128::from(latency);
        connection.latency_max = connection.latency_max.max(latency);
    }

    /// Record that `request` failed with `kind` at `now`.
    pub fn fail(
        &mut self,
        timing: &Timing,
        request: &Request,
        now: u64,
        kind: ErrorKind,
        connection: &mut ConnectionCounters,
    ) {
        self.last_outcome = self.last_outcome.max(Some(now));
        let period = timing.period(request, now);
        self.periods[period as usize].errors[kind as usize] += 1;
        if period == Period::Measured {
            connection.errors += 1;
        }
    }

    /// Record `count` planned requests of `period` that were never sent.
    pub fn unsent(&mut self, period: Period, count: u64) {
        self.periods[period as usize].errors[ErrorKind::Unsent as usize] += count;
    }

    pub fn merge(&mut self, other: &Self) {
        for (outcomes, other) in self.periods.iter_mut().zip(&other.periods) {
            outcomes.merge(other);
        }
        self.distributions.merge(&other.distributions);
        for (interval, other) in self.intervals.iter_mut().zip(&other.intervals) {
            interval.completions += other.completions;
            interval.latency.merge(&other.latency);
        }
        self.sent += other.sent;
        self.sent_in_window += other.sent_in_window;
        self.completed_in_window += other.completed_in_window;
        self.last_outcome = self.last_outcome.max(other.last_outcome);
        self.connects += other.connects;
        self.connect_errors += other.connect_errors;
        self.disconnects += other.disconnects;
        self.idle_closes += other.idle_closes;
        self.write_would_block += other.write_would_block;
        self.bytes_written += other.bytes_written;
        self.bytes_read += other.bytes_read;
        self.iterations += other.iterations;
        self.busy += other.busy;
        self.cpu += other.cpu;
    }
}

#[cfg(test)]
mod tests;
