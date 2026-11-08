// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Runs against a small in-test server. Assertions are on counts, validation, and causality: a latency at least as long as a stall the server imposed, never an upper bound on how fast anything ran.

mod closed;
mod command;
mod errors;
mod features;
mod rate;
mod server;
mod setup;

use base_benchmark::{Measurement, ProcessRecord};
use base_benchmark_load::{Command, Options, Report, run};
use serde_json::Value;

pub const MS: f64 = 1_000_000.0;

pub fn options(arguments: &[&str]) -> Options {
    match Options::parse(arguments.iter().map(|argument| argument.to_string())).unwrap() {
        Command::Run(options) => *options,
        command => panic!("expected a run, not {command:?}"),
    }
}

/// Run `mode` against `address` with `arguments`, quietly.
pub fn load(
    mode: &str,
    address: std::net::SocketAddr,
    arguments: &[&str],
) -> std::io::Result<Report> {
    let address = address.to_string();
    let mut all = vec![mode, "--address", &address];
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
