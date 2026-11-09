// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{
    histogram::Histogram,
    options::{FailOn, Mode},
    statistics::{ConnectionCounters, ErrorKind, InvalidKind, Outcomes, Period, WorkerStatistics},
};
use base_benchmark::{Direction, Metric, Phase, ProcessRecord, Sample, Source, Unit};
use serde_json::{Map, Value, json};
use std::{fmt::Write as _, io};

/// The workload identity in every record.
pub const WORKLOAD: &str = "base-benchmark-load/http1";
/// The name of the record extension holding the details summaries do not compare.
pub const EXTENSION: &str = "base-benchmark-load";
/// The quantiles reported for every distribution, with their metric name suffixes.
const QUANTILES: [(&str, f64); 5] = [
    ("p50", 0.5),
    ("p90", 0.9),
    ("p99", 0.99),
    ("p999", 0.999),
    ("max", 1.0),
];
/// The distributions of the client's own delays, diagnostics of the client rather than properties of the server.
const CLIENT_DISTRIBUTIONS: [&str; 2] = ["dispatch_lag", "schedule_lag"];

/// The 64-bit FNV-1a digest of `bytes`, in hexadecimal: a compact identity for request templates and expected bodies.
pub fn digest(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// The spread of a value across connections or workers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spread {
    pub min: f64,
    pub median: f64,
    pub max: f64,
    pub mean: f64,
    /// The standard deviation divided by the mean, or zero when the mean is zero.
    pub coefficient_of_variation: f64,
    /// Jain's fairness index, `(Σx)² / (n Σx²)`: 1 when all are equal, `1/n` when one has everything. 1 when all are zero.
    pub jain: f64,
    /// How many are zero.
    pub zero: usize,
}

impl Spread {
    pub fn of(values: &[f64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let count = values.len() as f64;
        let sum: f64 = values.iter().sum();
        let squares: f64 = values.iter().map(|value| value * value).sum();
        let mean = sum / count;
        let variance = (squares / count - mean * mean).max(0.0);
        let middle = sorted.len() / 2;
        let median = if sorted.len().is_multiple_of(2) {
            (sorted[middle - 1] + sorted[middle]) / 2.0
        } else {
            sorted[middle]
        };
        Some(Self {
            min: sorted[0],
            median,
            max: sorted[sorted.len() - 1],
            mean,
            coefficient_of_variation: if mean > 0.0 {
                variance.sqrt() / mean
            } else {
                0.0
            },
            jain: if squares > 0.0 {
                sum * sum / (count * squares)
            } else {
                1.0
            },
            zero: values.iter().filter(|value| **value == 0.0).count(),
        })
    }

    fn to_json(self) -> Value {
        json!({
            "min": self.min,
            "median": self.median,
            "max": self.max,
            "mean": self.mean,
            "coefficient_of_variation": self.coefficient_of_variation,
            "jain": self.jain,
            "zero": self.zero,
        })
    }
}

/// The connections the server starved, and how long it kept them waiting.
///
/// A connection is starved when the server answered none of its requests of the measured period, with a valid or an invalid response: in the closed loop, none completed in the window; in the open loop, none of its requests planned in the window, if it had any. A connection is starved, too, when it had a **starved span** longer than `--starved-limit` of the window: a time during which its slot, across failures and reconnections, had a request outstanding or due to send, and the server sent it no response. A span counts from the window's start, and one open at the window's end runs on through the drain. A fair server answers every connection at least once a round trip, however slow it is, so its spans stay near its round trip, while a connection the server stops serving has a span from its last response to the end of the run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Starvation {
    /// The connections, starved or not.
    pub connections: usize,
    /// The starved connections, by either criterion.
    pub starved: usize,
    /// The connections whose requests of the measured period the server answered none of.
    pub unanswered: usize,
    /// The connections with a starved span longer than the limit.
    pub long_span: usize,
    /// The starved connections that the server answered in the window and whose starved spans were longer than the limit only with the drain: in the window, each was within the limit, so `--fail-on starved` ignores them, though a span may have been close to the limit at the window's end.
    pub drain_only: usize,
    /// The longest starved span of any connection.
    pub span_max: u64,
    /// The longest any request outstanding in the window waited for its response.
    pub wait_max: u64,
    /// Whether that request ended without a response, so that its wait is a lower bound.
    pub wait_unanswered: bool,
}

impl Starvation {
    /// The starved connections' share of the connections.
    pub fn fraction(&self) -> f64 {
        self.starved as f64 / self.connections as f64
    }

    /// The connections starved in the window, which `--fail-on starved` rejects: every starved connection but those starved only in the drain.
    pub fn in_window(&self) -> usize {
        self.starved - self.drain_only
    }
}

/// How one connection was starved, if it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Judgement {
    unanswered: bool,
    long_span: bool,
    drain_only: bool,
}

impl Judgement {
    fn starved(self) -> bool {
        self.unanswered || self.long_span
    }
}

/// One worker's results, as the report keeps them.
#[derive(Debug)]
pub struct WorkerReport {
    pub cpu: Option<usize>,
    /// The CPUs the worker could run on.
    pub affinity: Vec<usize>,
    pub statistics: WorkerStatistics,
    pub connections: Vec<ConnectionCounters>,
    /// The time its CPU spent in software interrupts over the controller's window, when it was pinned and `/proc/stat` could be read. Its thread's CPU time leaves this out on kernels that account interrupt time separately, though on loopback much of it is the kernel delivering the worker's own packets.
    pub softirq: Option<u64>,
}

/// Merge the workers' statistics exactly.
pub(crate) fn merge(workers: &[WorkerReport]) -> WorkerStatistics {
    let mut total = workers[0].statistics.clone();
    for worker in &workers[1..] {
        total.merge(&worker.statistics);
    }
    total
}

/// The results of a run.
#[derive(Debug)]
pub struct Report {
    pub(crate) mode: Mode,
    pub(crate) parameters: Value,
    pub(crate) signature: String,
    /// The controller's measurement of the window: wall time, process CPU time, and context switches.
    pub(crate) sample: Sample,
    pub(crate) workers: Vec<WorkerReport>,
    pub(crate) total: WorkerStatistics,
    /// The requests planned in the window, in the open loop.
    pub(crate) planned: u64,
    /// The length of the window, and when it ended, in nanoseconds of the run's clock.
    pub(crate) window: u64,
    pub(crate) window_end: u64,
    pub(crate) interval: u64,
    pub(crate) busy_limit: f64,
    pub(crate) schedule_lag_limit: u64,
    pub(crate) shortfall_limit: f64,
    /// The fraction of the window a connection's starved span may last without starving it.
    pub(crate) starved_limit: f64,
    pub(crate) connection_details: bool,
    /// The requests sent before the load, which have no outcome in the record.
    pub(crate) preflight_requests: u64,
    pub(crate) context: Value,
}

impl Report {
    pub fn statistics(&self) -> &WorkerStatistics {
        &self.total
    }

    /// The measured operations, which are successful requests only, as `base-benchmark` requires of a driver: the valid responses to requests planned in the window in the open loop, and the valid responses completed in the window in the closed loop. Planned, invalid, and failed requests are counted apart.
    pub fn operations(&self) -> u64 {
        self.total.period(Period::Measured).valid
    }

    /// The fraction of the window each worker spent in iterations that did work.
    pub fn busy_fractions(&self) -> Vec<f64> {
        self.workers
            .iter()
            .map(|worker| worker.statistics.busy as f64 / self.window as f64)
            .collect()
    }

    /// How many of the requests planned in the window were not sent in it, in the open loop: how much the backlog of requests due but unsent grew over the window. A run that keeps up has about the same backlog at both ends of it.
    pub fn shortfall(&self) -> u64 {
        match self.mode {
            Mode::Rate => self.planned.saturating_sub(self.total.sent_in_window),
            Mode::Closed => 0,
        }
    }

    /// The client's part of the [`Report::shortfall`]: how many fewer requests the workers reached on their schedule in the window than were planned in it, which is how much further behind their schedule they were at its end than at its start, up to the shortfall.
    ///
    /// Every request is reached before it is sent, so with `planned`, `reached`, and `sent` in the window, the shortfall, `planned - sent` or zero, is `planned - reached` plus `reached - sent`. Either difference is negative when the workers caught up, in the window, on a backlog from before it, so each part is its difference clamped to between zero and the shortfall, and the two parts always sum to the shortfall: the parts are the differences when both are positive, and otherwise the positive one, up to the shortfall, is all of it.
    pub fn schedule_shortfall(&self) -> u64 {
        self.planned
            .saturating_sub(self.total.reached_in_window)
            .min(self.shortfall())
    }

    /// The connections' part of the [`Report::shortfall`]: how many more requests the workers reached in the window than they sent in it, up to the shortfall, because their connections had no room, with as many requests outstanding as the depth allows, awaiting responses, or were reconnecting. It is the shortfall less the [`Report::schedule_shortfall`].
    pub fn connection_shortfall(&self) -> u64 {
        self.shortfall() - self.schedule_shortfall()
    }

    /// Whether `shortfall` is more than `--shortfall-limit` of the requests planned in the window, and its share of them.
    fn excessive(&self, shortfall: u64) -> Option<f64> {
        (shortfall as f64 > self.shortfall_limit * self.planned as f64)
            .then(|| shortfall as f64 / self.planned as f64)
    }

    /// The most any worker was late in reaching a request planned in the window, if any was.
    fn schedule_lag_max(&self) -> Option<u64> {
        self.total.distributions.schedule_lag.max()
    }

    /// Why the load may not be what the run claims because of the client: busy-polling uses every worker CPU whatever the load, so saturation is judged by busy time, by how late the workers reached due requests, and by whether they reached the window's requests in it.
    pub fn saturation(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        for (index, busy) in self.busy_fractions().into_iter().enumerate() {
            if busy > self.busy_limit {
                reasons.push(format!(
                    "worker {index} was busy for {:.1}% of the window",
                    busy * 100.0
                ));
            }
        }
        for (index, worker) in self.workers.iter().enumerate() {
            let lag = worker
                .statistics
                .distributions
                .schedule_lag
                .value_at_quantile(0.99);
            if let Some(lag) = lag.filter(|lag| *lag > self.schedule_lag_limit) {
                reasons.push(format!(
                    "worker {index}'s 99th percentile schedule lag, {}, exceeds {}",
                    micros(lag),
                    micros(self.schedule_lag_limit)
                ));
            }
        }
        let shortfall = self.schedule_shortfall();
        if let Some(share) = self.excessive(shortfall) {
            reasons.push(format!(
                "the workers reached {shortfall} fewer {} in the window than the {} planned in it ({:.2}%)",
                requests(shortfall),
                self.planned,
                share * 100.0
            ));
        }
        reasons
    }

    /// Why the window did not carry the offered rate other than because the workers fell behind their schedule, if it did not: the [`Report::connection_shortfall`], which is never more than the shortfall, is more than `--shortfall-limit` of the requests planned in the window. The workers reached those requests in the window, but could send them only after it, because their connections were awaiting responses or reconnecting. This is not the client's saturation, so `--fail-on saturated` ignores it.
    ///
    /// The reason names the server only when the client is not saturated and no worker reached any request planned in the window later than `--schedule-lag-limit`, since a worker that stalled, even briefly near the window's end, may have read late the responses that make room. Otherwise it names the connections. Either way it states how many requests the connections may have outstanding, and the round trip under which that many carry the offered rate, since connections too few or too shallow for the server's healthy round trip are full whatever the server does.
    pub fn held_back(&self) -> Option<String> {
        let shortfall = self.connection_shortfall();
        let share = self.excessive(shortfall)?;
        let count = |name: &str| {
            self.parameters[name]
                .as_u64()
                .expect("a rate-mode report's parameters have the rate, connections, and depth")
        };
        let (connections, depth, rate) = (count("connections"), count("depth"), count("rate"));
        let outstanding = connections * depth;
        let capacity = format!(
            "with {connections} × {depth} outstanding {}, the connections carry the offered {rate}/s only while the round trip stays under {}",
            requests(outstanding),
            micros((outstanding as f64 * 1e9 / rate as f64) as u64)
        );
        let held = format!(
            "held back {shortfall} {} ({:.2}% of the {} planned in the window): the workers reached them in the window, but sent them after it, since their connections were awaiting responses or reconnecting",
            requests(shortfall),
            share * 100.0,
            self.planned
        );
        let starvation = self.starvation();
        // Starved connections hold their requests back whatever the connections' number and depth.
        let starved = if starvation.starved > 0 {
            format!(
                "; {} of the {connections} connections {} starved, with requests the server left waiting",
                starvation.starved,
                were(starvation.starved)
            )
        } else {
            String::new()
        };
        let late = self
            .schedule_lag_max()
            .filter(|lag| *lag > self.schedule_lag_limit);
        let distrust = if !self.saturation().is_empty() {
            "the client was saturated".to_owned()
        } else if let Some(lag) = late {
            format!(
                "a worker reached a request {} late, more than {}",
                micros(lag),
                micros(self.schedule_lag_limit)
            )
        } else {
            return Some(format!(
                "the server {held}; {capacity}, so if the server's healthy round trip is longer, the run needs more connections or depth{starved}"
            ));
        };
        Some(format!(
            "the connections ({connections} × {depth} outstanding) {held}; the server is not named, since {distrust}, so the workers may have read late the responses that make room, and {capacity}{starved}"
        ))
    }

    /// The longest a connection's starved span may last without starving it: `--starved-limit` of the window.
    pub fn span_limit(&self) -> u64 {
        (self.starved_limit * self.window as f64) as u64
    }

    /// Whether the server answered none of a connection's requests of the measured period, and whether it had a starved span longer than the limit, only with the drain or in the window too: the two ways a connection is starved.
    fn judge(&self, counters: &ConnectionCounters) -> Judgement {
        let expected = match self.mode {
            Mode::Closed => true,
            Mode::Rate => counters.planned > 0,
        };
        let unanswered = expected && counters.valid + counters.invalid == 0;
        let limit = self.span_limit();
        let long_span = counters.span_max > limit;
        Judgement {
            unanswered,
            long_span,
            drain_only: long_span && !unanswered && counters.span_window_max <= limit,
        }
    }

    /// The connections the server starved, the longest starved span, and the longest it kept a request waiting. Starvation is the server's doing, never the client's saturation: a worker reads every response as it arrives, unless it stops running, which its busy time and schedule lag show.
    pub fn starvation(&self) -> Starvation {
        let mut starvation = Starvation::default();
        for (_, counters) in self.connections() {
            let judgement = self.judge(counters);
            starvation.connections += 1;
            starvation.starved += usize::from(judgement.starved());
            starvation.unanswered += usize::from(judgement.unanswered);
            starvation.long_span += usize::from(judgement.long_span);
            starvation.drain_only += usize::from(judgement.drain_only);
            starvation.span_max = starvation.span_max.max(counters.span_max);
            if counters.wait_max > starvation.wait_max {
                starvation.wait_max = counters.wait_max;
                starvation.wait_unanswered = counters.wait_unanswered;
            } else if counters.wait_max == starvation.wait_max {
                starvation.wait_unanswered |= counters.wait_unanswered;
            }
        }
        starvation
    }

    /// The longest wait for a response, in words: a lower bound when the request ended without one.
    fn longest_wait(starvation: &Starvation) -> String {
        if starvation.wait_unanswered {
            format!(
                "at least {}, and its request was never answered",
                micros(starvation.wait_max)
            )
        } else {
            micros(starvation.wait_max)
        }
    }

    /// The longest starved span and the longest wait for a response, in words.
    fn longest(starvation: &Starvation) -> String {
        format!(
            "the longest the server left a connection without a response was {}, and the longest wait for a response was {}",
            micros(starvation.span_max),
            Self::longest_wait(starvation)
        )
    }

    /// Why connections were starved, if any were: how many, by each criterion, those starved only in the drain, the longest starved span and wait, and whether the client's own delays may have lengthened them.
    pub fn starved(&self) -> Option<String> {
        let starvation = self.starvation();
        if starvation.starved == 0 {
            return None;
        }
        let mut criteria = Vec::new();
        if starvation.unanswered > 0 {
            criteria.push(format!(
                "the server answered none of the requests of {} of them in the window",
                starvation.unanswered
            ));
        }
        if starvation.long_span > 0 {
            criteria.push(format!(
                "the server left {} of them without a response, with a request outstanding or due, for longer than {} ({:.0}% of the {} window)",
                starvation.long_span,
                micros(self.span_limit()),
                self.starved_limit * 100.0,
                micros(self.window)
            ));
        }
        if starvation.drain_only > 0 {
            criteria.push(format!(
                "{} of them only in the drain, after the window: in the window, the server answered them, and their starved spans were within the limit",
                starvation.drain_only
            ));
        }
        // A fair server leaves each connection without a response for about a round trip, so a limit shorter than the typical one flags every connection.
        let median = self.total.distributions.round_trip.value_at_quantile(0.5);
        if let Some(median) = median.filter(|&median| self.span_limit() < median) {
            criteria.push(format!(
                "the limit is shorter than the median round trip, {}, so even a fair server would exceed it: lengthen the window or raise `--starved-limit`",
                micros(median)
            ));
        }
        // A worker that falls behind sends late, from the schedule in the open loop, and reads late, so its own delays count in the spans and waits.
        let client = if self.saturation().is_empty() {
            ""
        } else {
            "; the client was saturated, so its own delays, such as its schedule lag, may have lengthened these spans and waits"
        };
        Some(format!(
            "{} of {} connections ({:.2}%) {} starved: {}; {}{client}",
            starvation.starved,
            starvation.connections,
            starvation.fraction() * 100.0,
            were(starvation.starved),
            criteria.join("; "),
            Self::longest(&starvation)
        ))
    }

    fn all_outcomes(&self) -> Outcomes {
        let mut outcomes = Outcomes::default();
        for period in Period::ALL {
            outcomes.merge(self.total.period(period));
        }
        outcomes
    }

    /// The outcomes `fail_on` names, in every period.
    pub fn failures(&self, fail_on: FailOn) -> Vec<String> {
        let outcomes = self.all_outcomes();
        let mut failures = Vec::new();
        if fail_on.invalid && outcomes.invalid_total() > 0 {
            failures.push(format!("{} invalid responses", outcomes.invalid_total()));
        }
        let errors = outcomes.error_total() + self.total.connect_errors;
        if fail_on.errors && errors > 0 {
            failures.push(format!(
                "{} failed requests and {} failed connection attempts",
                outcomes.error_total(),
                self.total.connect_errors
            ));
        }
        if fail_on.saturated {
            failures.extend(
                self.saturation()
                    .into_iter()
                    .map(|reason| format!("the client was saturated: {reason}")),
            );
        }
        // Starvation only in the drain stayed within the limit in the window, so it is reported but does not fail the run.
        if fail_on.starved && self.starvation().in_window() > 0 {
            failures.extend(self.starved());
        }
        failures
    }

    fn distributions(&self) -> Vec<(&'static str, &Histogram)> {
        let distributions = &self.total.distributions;
        match self.mode {
            Mode::Rate => vec![
                ("latency", &distributions.latency),
                ("round_trip", &distributions.round_trip),
                ("send_delay", &distributions.send_delay),
                ("dispatch_lag", &distributions.dispatch_lag),
                ("schedule_lag", &distributions.schedule_lag),
            ],
            Mode::Closed => vec![
                ("service_time", &distributions.round_trip),
                ("dispatch_lag", &distributions.dispatch_lag),
            ],
        }
    }

    /// The `base-benchmark` process record, with one sample over the measured window.
    ///
    /// Totals of the measured phase are divided by the operations in `base-benchmark` summaries, and only properties of the server have a direction: its timings, wall time per success, invalid responses, and errors are lower-is-better. Diagnostics of the run, such as the client's CPU time, context switches, dispatch and schedule lag, busy time, the shortfalls, and saturation, are neutral, since a faster server makes the client busier.
    ///
    /// Fails when there are no operations to normalize by: a run without a single valid response in the measured period.
    pub fn record(&self) -> io::Result<ProcessRecord> {
        let operations = self.operations();
        if operations == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                match self.mode {
                    Mode::Rate => "no valid response to a request planned in the measured window",
                    Mode::Closed => "no valid response completed in the measured window",
                },
            ));
        }
        let mut sample = self.sample.clone();
        sample.operations = operations;
        // The controller's interval measures this process: its CPU time and context switches describe the client, not the server, so they are neutral. Its wall time per success is the inverse of the throughput, and keeps its direction.
        for metric in &mut sample.metrics {
            if metric.source == Source::Rusage {
                metric.direction = Direction::Neutral;
            }
        }
        let measured = self.total.period(Period::Measured);
        let count = |name: &str, value: u64| Metric::total(name, Unit::Count, value);
        sample.push(count("responses_valid", measured.valid));
        if self.mode == Mode::Rate {
            sample.push(count("requests_planned", self.planned));
        }
        sample.push(count("responses_invalid", measured.invalid_total()).lower_is_better());
        for kind in InvalidKind::ALL {
            sample.push(
                count(
                    &format!("invalid_{}", kind.name()),
                    measured.invalid[kind as usize],
                )
                .lower_is_better(),
            );
        }
        sample.push(count("errors", measured.error_total()).lower_is_better());
        for kind in ErrorKind::ALL {
            sample.push(
                count(
                    &format!("errors_{}", kind.name()),
                    measured.errors[kind as usize],
                )
                .lower_is_better(),
            );
        }
        sample.push(
            count("connect_errors", self.total.connect_errors)
                .with_phase(Phase::Process)
                .lower_is_better(),
        );
        // Per operation, these are the completions and sends per valid response, which describe the run rather than rank servers; throughput is the inverse of `wall_time` per operation.
        sample.push(count("completed_in_window", self.total.completed_in_window));
        sample.push(count("sent_in_window", self.total.sent_in_window));
        if self.mode == Mode::Rate {
            sample.push(count("sent_in_window_shortfall", self.shortfall()));
            sample.push(count("schedule_shortfall", self.schedule_shortfall()));
            sample.push(count("connection_shortfall", self.connection_shortfall()));
        }
        // Every valid response of the measured period is in every distribution, and the schedule lag holds every request planned in the window, which a valid response of the cohort was, so none is empty.
        const NONEMPTY: &str = "a record has valid responses, which every distribution holds";
        // The server's timings are lower-is-better; the client's delays are neutral.
        let direction = |name: &str, metric: Metric| {
            if CLIENT_DISTRIBUTIONS.contains(&name) {
                metric
            } else {
                metric.lower_is_better()
            }
        };
        for (name, histogram) in self.distributions() {
            for (suffix, quantile) in QUANTILES {
                let value = histogram.value_at_quantile(quantile).expect(NONEMPTY);
                sample.push(direction(
                    name,
                    Metric::quantile(
                        format!("{name}_{suffix}"),
                        Unit::Nanoseconds,
                        quantile,
                        value as f64,
                    ),
                ));
            }
            let mean = histogram.mean().expect(NONEMPTY);
            sample.push(direction(
                name,
                Metric::mean(format!("{name}_mean"), Unit::Nanoseconds, mean),
            ));
        }
        let drain = self
            .total
            .last_outcome
            .map_or(0, |last| last.saturating_sub(self.window_end));
        sample.push(
            Metric::mean("drain_time", Unit::Nanoseconds, drain as f64).with_phase(Phase::Attached),
        );
        // The client's saturation and busy time qualify the measurement rather than the server: in the closed loop, a faster server makes the client busier. They are neutral, so summaries report them without ranking subjects by them.
        sample.push(Metric::mean(
            "client_saturated",
            Unit::Count,
            f64::from(!self.saturation().is_empty() as u8),
        ));
        let busiest = self
            .workers
            .iter()
            .map(|worker| worker.statistics.busy)
            .max()
            .expect("every run has workers");
        sample.push(Metric::mean(
            "worker_busy_time_max",
            Unit::Nanoseconds,
            busiest as f64,
        ));
        // Starvation is the server's doing, so fewer starved connections and shorter waits are better.
        let starvation = self.starvation();
        sample.push(
            Metric::mean(
                "connections_starved",
                Unit::Count,
                starvation.starved as f64,
            )
            .lower_is_better(),
        );
        sample.push(
            Metric::mean(
                "connections_starved_fraction",
                Unit::Count,
                starvation.fraction(),
            )
            .lower_is_better(),
        );
        // The longest starved span and wait run on into the drain, past the measured window.
        for (name, value) in [
            ("starved_span_max", starvation.span_max),
            ("response_wait_max", starvation.wait_max),
        ] {
            sample.push(
                Metric::quantile(name, Unit::Nanoseconds, 1.0, value as f64)
                    .with_phase(Phase::Attached)
                    .lower_is_better(),
            );
        }
        let completions: Vec<f64> = self
            .connections()
            .map(|(_, counters)| counters.valid as f64)
            .collect();
        let completions = Spread::of(&completions).expect("every run has connections");
        sample.push(Metric::mean(
            "connection_completions_min",
            Unit::Count,
            completions.min,
        ));
        sample.push(Metric::mean(
            "connection_completions_max",
            Unit::Count,
            completions.max,
        ));
        Ok(ProcessRecord::new(WORKLOAD, self.parameters.clone())
            .with_signature(self.signature.clone())
            .with_sample(sample)
            .with_extension(EXTENSION, self.extension()))
    }

    fn connections(&self) -> impl Iterator<Item = (usize, &ConnectionCounters)> {
        self.workers.iter().enumerate().flat_map(|(index, worker)| {
            worker
                .connections
                .iter()
                .map(move |counters| (index, counters))
        })
    }

    fn extension(&self) -> Value {
        let outcomes = |outcomes: &Outcomes| {
            let invalid: Map<String, Value> = InvalidKind::ALL
                .iter()
                .map(|kind| {
                    (
                        kind.name().to_owned(),
                        outcomes.invalid[*kind as usize].into(),
                    )
                })
                .collect();
            let errors: Map<String, Value> = ErrorKind::ALL
                .iter()
                .map(|kind| {
                    (
                        kind.name().to_owned(),
                        outcomes.errors[*kind as usize].into(),
                    )
                })
                .collect();
            json!({"valid": outcomes.valid, "invalid": invalid, "errors": errors})
        };
        let periods: Map<String, Value> = Period::ALL
            .iter()
            .map(|period| {
                (
                    period.name().to_owned(),
                    outcomes(self.total.period(*period)),
                )
            })
            .collect();
        let completions: Vec<f64> = self
            .connections()
            .map(|(_, counters)| counters.valid as f64)
            .collect();
        let latencies: Vec<f64> = self
            .connections()
            .filter(|(_, counters)| counters.valid > 0)
            .map(|(_, counters)| counters.latency_sum as f64 / counters.valid as f64)
            .collect();
        let worker_completions: Vec<f64> = self
            .workers
            .iter()
            .map(|worker| worker.statistics.completed_in_window as f64)
            .collect();
        let spread = |values: &[f64]| Spread::of(values).map_or(Value::Null, Spread::to_json);
        let busy = self.busy_fractions();
        let workers: Vec<Value> = self
            .workers
            .iter()
            .zip(&busy)
            .map(|(worker, busy)| {
                let statistics = &worker.statistics;
                json!({
                    "cpu": worker.cpu,
                    "affinity": worker.affinity,
                    "busy_fraction": busy,
                    "busy_ns": statistics.busy,
                    "cpu_ns": statistics.cpu,
                    "iterations": statistics.iterations,
                    "sent": statistics.sent,
                    "completed_in_window": statistics.completed_in_window,
                    "softirq_ns": worker.softirq,
                    "dispatch_lag_p99_ns": statistics.distributions.dispatch_lag.value_at_quantile(0.99),
                    "schedule_lag_p99_ns": statistics.distributions.schedule_lag.value_at_quantile(0.99),
                })
            })
            .collect();
        let intervals: Vec<Value> = self
            .total
            .intervals
            .iter()
            .enumerate()
            .map(|(index, interval)| {
                json!({
                    "offset_ns": index as u64 * self.interval,
                    "completions": interval.completions,
                    "latency_count": interval.latency.count(),
                    "latency_p50_ns": interval.latency.value_at_quantile(0.5),
                    "latency_p99_ns": interval.latency.value_at_quantile(0.99),
                    "latency_max_ns": interval.latency.max(),
                })
            })
            .collect();
        let histograms: Map<String, Value> = self
            .distributions()
            .into_iter()
            .map(|(name, histogram)| (name.to_owned(), histogram.to_json()))
            .collect();
        let saturation = self.saturation();
        let starvation = self.starvation();
        let mut extension = json!({
            "context": self.context,
            "planned": (self.mode == Mode::Rate).then_some(self.planned),
            "preflight_requests": self.preflight_requests,
            "periods": periods,
            "connections": {
                "connects": self.total.connects,
                "connect_errors": self.total.connect_errors,
                "disconnects": self.total.disconnects,
                "idle_closes": self.total.idle_closes,
            },
            "bytes_written": self.total.bytes_written,
            "bytes_read": self.total.bytes_read,
            "write_would_block": self.total.write_would_block,
            "fairness": {
                "connection_completions": spread(&completions),
                "connection_mean_latency_ns": spread(&latencies),
                "worker_completions": spread(&worker_completions),
            },
            "saturation": {
                "saturated": !saturation.is_empty(),
                "reasons": saturation,
                "busy_limit": self.busy_limit,
                "schedule_lag_limit_ns": (self.mode == Mode::Rate).then_some(self.schedule_lag_limit),
                "shortfall_limit": (self.mode == Mode::Rate).then_some(self.shortfall_limit),
            },
            "starvation": {
                "connections": starvation.connections,
                "starved": starvation.starved,
                "fraction": starvation.fraction(),
                "unanswered": starvation.unanswered,
                "long_span": starvation.long_span,
                "drain_only": starvation.drain_only,
                "limit": self.starved_limit,
                "span_limit_ns": self.span_limit(),
                "span_max_ns": starvation.span_max,
                "wait_max_ns": starvation.wait_max,
                "wait_max_unanswered": starvation.wait_unanswered,
                "reason": self.starved(),
            },
            "shortfall": (self.mode == Mode::Rate).then(|| json!({
                "sent": self.shortfall(),
                "schedule": self.schedule_shortfall(),
                "connection": self.connection_shortfall(),
                "held_back": self.held_back(),
            })),
            "workers": workers,
            "intervals": intervals,
            "histograms": histograms,
        });
        if self.connection_details {
            let details: Vec<Value> = self
                .connections()
                .map(|(worker, counters)| {
                    let judgement = self.judge(counters);
                    json!({
                        "worker": worker,
                        "valid": counters.valid,
                        "invalid": counters.invalid,
                        "errors": counters.errors,
                        "mean_latency_ns": (counters.valid > 0).then(|| counters.latency_sum as f64 / counters.valid as f64),
                        "max_latency_ns": counters.latency_max,
                        "bytes_read": counters.bytes_read,
                        "planned": (self.mode == Mode::Rate).then_some(counters.planned),
                        "starved": judgement.starved(),
                        "drain_only": judgement.drain_only,
                        "span_max_ns": counters.span_max,
                        "span_window_max_ns": counters.span_window_max,
                        "wait_max_ns": counters.wait_max,
                        "wait_max_unanswered": counters.wait_unanswered,
                    })
                })
                .collect();
            extension["connection_details"] = details.into();
        }
        extension
    }

    /// A human summary of the run, for stderr.
    pub fn summary(&self) -> String {
        let mut text = String::new();
        let outcomes = self.total.period(Period::Measured);
        let seconds = self.window as f64 / 1e9;
        let _ = writeln!(text, "{}", self.parameters_line());
        match self.mode {
            Mode::Rate => {
                let _ = writeln!(
                    text,
                    "measured cohort: {} planned; {} valid, {} invalid, {} failed",
                    self.planned,
                    outcomes.valid,
                    outcomes.invalid_total(),
                    outcomes.error_total()
                );
            }
            Mode::Closed => {
                let _ = writeln!(
                    text,
                    "measured window: {} valid, {} invalid, {} failed",
                    outcomes.valid,
                    outcomes.invalid_total(),
                    outcomes.error_total()
                );
                // The requests in flight at the window's end are drained, not abandoned, and their outcomes are not the window's, so they are stated apart.
                let drain = self.total.period(Period::Drain);
                let _ = writeln!(
                    text,
                    "in flight at the window's end, then drained: {} valid, {} invalid, {} failed",
                    drain.valid,
                    drain.invalid_total(),
                    drain.error_total()
                );
            }
        }
        // Starvation comes before the distributions, which leave out every request the server never answered.
        match self.starved() {
            Some(reason) => {
                let _ = writeln!(text, "STARVED: {reason}");
            }
            None => {
                let starvation = self.starvation();
                let _ = writeln!(
                    text,
                    "starved: none of {} connections; {}",
                    starvation.connections,
                    Self::longest(&starvation)
                );
            }
        }
        let drain = self
            .total
            .last_outcome
            .map_or(0, |last| last.saturating_sub(self.window_end));
        let _ = writeln!(
            text,
            "in the window: {} sent, {} completed ({:.0}/s); drained in {}",
            self.total.sent_in_window,
            self.total.completed_in_window,
            self.total.completed_in_window as f64 / seconds,
            micros(drain)
        );
        for (name, histogram) in self.distributions() {
            let values: Vec<String> = QUANTILES
                .iter()
                .map(|(suffix, quantile)| {
                    let value = histogram
                        .value_at_quantile(*quantile)
                        .map_or("-".to_owned(), micros);
                    format!("{suffix} {value}")
                })
                .collect();
            let _ = writeln!(text, "{name}: {}", values.join(", "));
        }
        let errors: Vec<String> = ErrorKind::ALL
            .iter()
            .filter(|kind| outcomes.errors[**kind as usize] > 0)
            .map(|kind| format!("{} {}", outcomes.errors[*kind as usize], kind.name()))
            .chain(
                InvalidKind::ALL
                    .iter()
                    .filter(|kind| outcomes.invalid[**kind as usize] > 0)
                    .map(|kind| {
                        format!(
                            "{} invalid ({})",
                            outcomes.invalid[*kind as usize],
                            kind.name()
                        )
                    }),
            )
            .collect();
        if !errors.is_empty() {
            let _ = writeln!(text, "problems: {}", errors.join(", "));
        }
        let busy: Vec<String> = self
            .busy_fractions()
            .iter()
            .map(|busy| format!("{:.0}%", busy * 100.0))
            .collect();
        let saturation = self.saturation();
        let verdict = if saturation.is_empty() {
            "not saturated".to_owned()
        } else {
            format!("SATURATED: {}", saturation.join("; "))
        };
        let _ = writeln!(text, "workers busy: {}; {verdict}", busy.join(" "));
        if let Some(reason) = self.held_back() {
            let _ = writeln!(text, "shortfall: {reason}");
        }
        text
    }

    fn parameters_line(&self) -> String {
        let parameters = &self.parameters;
        let load = match self.mode {
            Mode::Rate => format!("{}/s offered", parameters["rate"]),
            Mode::Closed => "closed loop".to_owned(),
        };
        format!(
            "base-benchmark-load {}: {load} over {} connections, depth {}, {} workers; {} warmup, {} measured",
            self.mode.name(),
            parameters["connections"],
            parameters["depth"],
            parameters["workers"],
            micros(parameters["warmup_ns"].as_u64().unwrap_or(0)),
            micros(self.window),
        )
    }
}

/// The noun for `count` requests.
fn requests(count: u64) -> &'static str {
    if count == 1 { "request" } else { "requests" }
}

/// The verb for `count` connections that were something.
fn were(count: usize) -> &'static str {
    if count == 1 { "was" } else { "were" }
}

/// A duration in nanoseconds, in readable units.
pub fn micros(nanoseconds: u64) -> String {
    let value = nanoseconds as f64;
    if nanoseconds >= 1_000_000_000 {
        format!("{:.3} s", value / 1e9)
    } else if nanoseconds >= 1_000_000 {
        format!("{:.3} ms", value / 1e6)
    } else {
        format!("{:.2} µs", value / 1e3)
    }
}

#[cfg(test)]
mod tests;
