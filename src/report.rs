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

/// One worker's results, as the report keeps them.
#[derive(Debug)]
pub struct WorkerReport {
    pub cpu: Option<usize>,
    /// The CPUs the worker could run on.
    pub affinity: Vec<usize>,
    pub statistics: WorkerStatistics,
    pub connections: Vec<ConnectionCounters>,
    /// The time its CPU spent in software interrupts over the controller's window, when it was pinned and `/proc/stat` could be read. Its thread's CPU time leaves this out, though on loopback much of it is the kernel delivering the worker's own packets.
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

    /// Why the load may not be what the run claims: busy-polling uses every worker CPU whatever the load, so saturation is judged by busy time, by how late the workers reached due requests, and by whether the window's requests were sent in it.
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
        let shortfall = self.shortfall();
        if shortfall as f64 > self.shortfall_limit * self.planned as f64 {
            reasons.push(format!(
                "{shortfall} of the {} requests planned in the window ({:.2}%) were not sent in it",
                self.planned,
                shortfall as f64 * 100.0 / self.planned as f64
            ));
        }
        reasons
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
    /// Totals of the measured phase are divided by the operations in `base-benchmark` summaries, and only properties of the server have a direction: its timings, wall time per success, invalid responses, and errors are lower-is-better. Diagnostics of the client, such as its CPU time, context switches, dispatch and schedule lag, busy time, the shortfall, and saturation, are neutral, since a faster server makes the client busier.
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
                "shortfall": (self.mode == Mode::Rate).then_some(self.shortfall()),
            },
            "workers": workers,
            "intervals": intervals,
            "histograms": histograms,
        });
        if self.connection_details {
            let details: Vec<Value> = self
                .connections()
                .map(|(worker, counters)| {
                    json!({
                        "worker": worker,
                        "valid": counters.valid,
                        "invalid": counters.invalid,
                        "errors": counters.errors,
                        "mean_latency_ns": (counters.valid > 0).then(|| counters.latency_sum as f64 / counters.valid as f64),
                        "max_latency_ns": counters.latency_max,
                        "bytes_read": counters.bytes_read,
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
