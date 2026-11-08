// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Runs against a small in-test server. Assertions are on counts, validation, and causality: a latency at least as long as a stall the server imposed, never an upper bound on how fast anything ran.
//!
//! No assertion depends on how much happened in a short time. An open loop ends once every planned request has an outcome, so its counts are exact however slowly it ran; the response, connect, and drain timeouts are minutes long, as [`WATCHDOGS`], unless a test is about them, and then it asserts only what the timeout causes. A closed loop sends only in its window, so the tests that need a request sent or a valid response in one use [`CLOSED_WINDOW`]; those with a shorter window assert only what holds when nothing was sent.

mod closed;
mod command;
mod errors;
mod features;
mod placement;
mod rate;
mod server;
mod setup;

use base_benchmark::{Measurement, ProcessRecord};
use base_benchmark_load::{Command, ErrorKind, Options, Outcomes, Period, Report, run};
use serde_json::Value;

pub const MS: f64 = 1_000_000.0;

/// The window of a closed loop that must send a request, or complete a valid response, in it: long enough to be only a watchdog, since the client or server would have to stall for all of it to leave it empty.
pub const CLOSED_WINDOW: &str = "2s";

pub fn options(arguments: &[&str]) -> Options {
    match Options::parse(arguments.iter().map(|argument| argument.to_string())).unwrap() {
        Command::Run(options) => *options,
        command => panic!("expected a run, not {command:?}"),
    }
}

/// The response, connect, and drain timeouts of every run, unless its arguments give their own: only watchdogs, so that a run starved of CPU, as on one CPU shared with dozens of busy processes, still gives every request an outcome before they expire. The options' defaults of 10 to 15 seconds are not enough there.
const WATCHDOGS: [&str; 6] = [
    "--timeout",
    "300s",
    "--connect-timeout",
    "300s",
    "--drain-timeout",
    "300s",
];

/// Run `mode` against `address` with `arguments`, quietly, and with [`WATCHDOGS`] for the timeouts the arguments do not give.
pub fn load(
    mode: &str,
    address: std::net::SocketAddr,
    arguments: &[&str],
) -> std::io::Result<Report> {
    let address = address.to_string();
    let mut all = vec![mode, "--address", &address];
    // Later arguments override earlier ones.
    all.extend_from_slice(&WATCHDOGS);
    all.extend_from_slice(arguments);
    run(&options(&all))
}

pub fn record(report: &Report) -> ProcessRecord {
    let record = report.record().unwrap();
    record.validate().unwrap();
    record
}

/// A metric's value: a total, mean, or quantile.
pub fn metric(record: &ProcessRecord, name: &str) -> f64 {
    let metric = record.samples[0]
        .metric(name)
        .unwrap_or_else(|| panic!("no metric {name}"));
    match &metric.measurement {
        Measurement::Total { value } => *value as f64,
        Measurement::Mean { value } | Measurement::Quantile { value, .. } => *value,
        other => panic!("{name} is {other:?}"),
    }
}

pub fn count(record: &ProcessRecord, name: &str) -> u64 {
    metric(record, name) as u64
}

pub fn extension(record: &ProcessRecord) -> &Value {
    &record.extensions[base_benchmark_load::EXTENSION]
}

/// Every request of `period` with an outcome: valid, invalid, or failed.
pub fn outcomes(record: &ProcessRecord, period: &str) -> u64 {
    let period = &extension(record)["periods"][period];
    let sum = |value: &Value| {
        value
            .as_object()
            .unwrap()
            .values()
            .map(|count| count.as_u64().unwrap())
            .sum::<u64>()
    };
    period["valid"].as_u64().unwrap() + sum(&period["invalid"]) + sum(&period["errors"])
}

/// The outcomes of a report's measured period, for runs that have no record because none was valid.
pub fn measured(report: &Report) -> Outcomes {
    *report.statistics().period(Period::Measured)
}

pub fn errors(outcomes: &Outcomes, kind: ErrorKind) -> u64 {
    outcomes.errors[kind as usize]
}

/// The requests a server received for a record: every outcome, and the preflight requests, which have none.
pub fn requests(record: &ProcessRecord) -> u64 {
    let preflight = extension(record)["preflight_requests"].as_u64().unwrap();
    preflight
        + outcomes(record, "warmup")
        + outcomes(record, "measured")
        + outcomes(record, "drain")
}
