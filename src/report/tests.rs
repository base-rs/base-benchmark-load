// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use crate::statistics::{Request, Timing};
use base_benchmark::{Direction, Measurement};

const MS: u64 = 1_000_000;

fn timing(closed: bool) -> Timing {
    Timing {
        closed,
        start: 0,
        window_start: 10 * MS,
        window_end: 20 * MS,
        interval: 5 * MS,
    }
}

/// A report of `mode` with two workers whose statistics `fill` sets.
fn report(
    mode: Mode,
    fill: impl Fn(usize, &mut WorkerStatistics, &mut ConnectionCounters),
) -> Report {
    let timing = timing(mode == Mode::Closed);
    let workers: Vec<WorkerReport> = (0..2)
        .map(|index| {
            let mut statistics = WorkerStatistics::new(&timing);
            let mut connection = ConnectionCounters::default();
            fill(index, &mut statistics, &mut connection);
            WorkerReport {
                cpu: Some(index + 2),
                affinity: vec![index + 2],
                statistics,
                connections: vec![connection, ConnectionCounters::default()],
            }
        })
        .collect();
    Report {
        mode,
        parameters: json!({"rate": 1000, "connections": 4, "depth": 1, "workers": 2, "warmup_ns": 10 * MS}),
        signature: "http1 status 200 body any".to_owned(),
        sample: Sample::new(1),
        total: merge(&workers),
        workers,
        planned: 10,
        window: 10 * MS,
        window_end: 20 * MS,
        interval: 5 * MS,
        busy_limit: 0.9,
        dispatch_lag_limit: None,
        connection_details: false,
        preflight_requests: 1,
        context: json!({"version": "test"}),
    }
}

fn complete(
    timing: &Timing,
    statistics: &mut WorkerStatistics,
    connection: &mut ConnectionCounters,
    planned: u64,
    now: u64,
) {
    let request = Request {
        ordinal: 0,
        planned,
        sent: planned + 100,
        eligible: planned + 50,
    };
    statistics.complete(timing, &request, now, None, connection);
}

fn value(record: &ProcessRecord, name: &str) -> Measurement {
    record.samples[0]
        .metric(name)
        .unwrap_or_else(|| panic!("{name}"))
        .measurement
        .clone()
}

#[test]
fn digests_are_fnv_1a() {
    assert_eq!(digest(b""), "cbf29ce484222325");
    assert_eq!(digest(b"a"), "af63dc4c8601ec8c");
}

#[test]
fn spreads() {
    assert_eq!(Spread::of(&[]), None);
    let even = Spread::of(&[4.0, 1.0, 3.0, 0.0]).unwrap();
    assert_eq!(
        (even.min, even.median, even.max, even.mean, even.zero),
        (0.0, 2.0, 4.0, 2.0, 1)
    );
    assert!((even.jain - 64.0 / (4.0 * 26.0)).abs() < 1e-12);
    assert!((even.coefficient_of_variation - 2.5f64.sqrt() / 2.0).abs() < 1e-12);
    let odd = Spread::of(&[5.0, 5.0, 5.0]).unwrap();
    assert_eq!(
        (odd.median, odd.jain, odd.coefficient_of_variation),
        (5.0, 1.0, 0.0)
    );
    let zero = Spread::of(&[0.0, 0.0]).unwrap();
    assert_eq!(
        (zero.jain, zero.coefficient_of_variation, zero.zero),
        (1.0, 0.0, 2)
    );
    assert_eq!(zero.to_json()["jain"], 1.0);
}

#[test]
fn durations_read_well() {
    assert_eq!(micros(1500), "1.50 µs");
    assert_eq!(micros(2_500_000), "2.500 ms");
    assert_eq!(micros(3_000_000_000), "3.000 s");
}

#[test]
fn open_loop_records() {
    let timing = timing(false);
    let mut report = report(Mode::Rate, |index, statistics, connection| {
        for planned in [11, 12, 13, 14, 15] {
            complete(
                &timing,
                statistics,
                connection,
                planned * MS,
                planned * MS + 1000 * (index as u64 + 1),
            );
        }
        statistics.busy = 2 * MS;
        statistics.last_outcome = Some(21 * MS);
    });
    // The controller's interval, as a run measures it.
    report.sample = base_benchmark::Interval::start()
        .unwrap()
        .finish(1)
        .unwrap();
    assert_eq!(report.operations(), 10);
    assert_eq!(report.busy_fractions(), [0.2, 0.2]);
    assert!(report.saturation().is_empty());
    let record = report.record().unwrap();
    record.validate().unwrap();
    assert_eq!(record.samples[0].operations, 10);
    assert_eq!(
        value(&record, "responses_valid"),
        Measurement::Total { value: 10 }
    );
    assert_eq!(
        value(&record, "requests_planned"),
        Measurement::Total { value: 10 }
    );
    // Only properties of the server rank subjects; the client's diagnostics are neutral.
    let direction = |name: &str| record.samples[0].metric(name).unwrap().direction;
    for name in [
        "responses_valid",
        "requests_planned",
        "completed_in_window",
        "sent_in_window",
        "client_saturated",
        "worker_busy_time_max",
        "connection_completions_min",
        "drain_time",
        "dispatch_lag_p99",
        "dispatch_lag_mean",
        "cpu_time",
        "user_cpu_time",
        "system_cpu_time",
        "voluntary_context_switches",
        "involuntary_context_switches",
    ] {
        assert_eq!(direction(name), Direction::Neutral, "{name}");
    }
    for name in [
        "wall_time",
        "responses_invalid",
        "errors",
        "errors_timeout",
        "connect_errors",
        "latency_p99",
        "latency_mean",
        "round_trip_p99",
        "send_delay_max",
    ] {
        assert_eq!(direction(name), Direction::LowerIsBetter, "{name}");
    }
    assert_eq!(
        value(&record, "latency_max"),
        Measurement::Quantile {
            quantile: 1.0,
            value: 2000.0
        }
    );
    assert_eq!(
        value(&record, "latency_mean"),
        Measurement::Mean { value: 1500.0 }
    );
    assert_eq!(
        value(&record, "dispatch_lag_p50"),
        Measurement::Quantile {
            quantile: 0.5,
            value: 50.0
        }
    );
    assert_eq!(
        value(&record, "drain_time"),
        Measurement::Mean { value: MS as f64 }
    );
    assert_eq!(
        value(&record, "client_saturated"),
        Measurement::Mean { value: 0.0 }
    );
    assert_eq!(
        value(&record, "worker_busy_time_max"),
        Measurement::Mean {
            value: 2.0 * MS as f64
        }
    );
    assert_eq!(
        value(&record, "connection_completions_min"),
        Measurement::Mean { value: 0.0 }
    );
    assert_eq!(
        value(&record, "connection_completions_max"),
        Measurement::Mean { value: 5.0 }
    );
    assert!(record.samples[0].metric("service_time_p50").is_none());
    let extension = &record.extensions[EXTENSION];
    assert_eq!(extension["planned"], 10);
    assert_eq!(extension["workers"][1]["cpu"], 3);
    assert_eq!(extension["workers"][1]["affinity"], json!([3]));
    assert_eq!(extension["preflight_requests"], 1);
    assert_eq!(extension["intervals"][0]["latency_count"], 8);
    assert_eq!(extension["intervals"][1]["offset_ns"], 5 * MS);
    assert_eq!(extension["fairness"]["connection_completions"]["zero"], 2);
    assert_eq!(
        extension["fairness"]["connection_mean_latency_ns"]["min"],
        1000.0
    );
    assert!(extension.get("connection_details").is_none());
    let summary = report.summary();
    assert!(summary.starts_with("base-benchmark-load rate: 1000/s offered over 4 connections, depth 1, 2 workers; 10.000 ms warmup, 10.000 ms measured"), "{summary}");
    assert!(summary.contains("measured cohort: 10 planned; 10 valid"));
    assert!(summary.contains("latency: p50 1.00 µs"));
    assert!(summary.contains("drained in 1.000 ms"));
    assert!(summary.contains("workers busy: 20% 20%; not saturated"));
    assert!(!summary.contains("problems"));
}

#[test]
fn closed_loop_records_and_details() {
    let timing = timing(true);
    let mut report = report(Mode::Closed, |index, statistics, connection| {
        if index == 0 {
            complete(&timing, statistics, connection, 0, 12 * MS);
            statistics.fail(
                &timing,
                &Request {
                    ordinal: 0,
                    planned: 0,
                    sent: 13 * MS,
                    eligible: 0,
                },
                14 * MS,
                ErrorKind::Reset,
                connection,
            );
        }
    });
    report.connection_details = true;
    assert_eq!(report.operations(), 1);
    let record = report.record().unwrap();
    assert!(record.samples[0].metric("latency_p50").is_none());
    assert!(record.samples[0].metric("requests_planned").is_none());
    let direction = |name: &str| record.samples[0].metric(name).unwrap().direction;
    assert_eq!(direction("service_time_p99"), Direction::LowerIsBetter);
    assert_eq!(direction("dispatch_lag_p99"), Direction::Neutral);
    assert_eq!(
        value(&record, "service_time_max"),
        Measurement::Quantile {
            quantile: 1.0,
            value: (12 * MS - 100) as f64
        }
    );
    assert_eq!(
        value(&record, "errors_reset"),
        Measurement::Total { value: 1 }
    );
    assert_eq!(
        value(&record, "drain_time"),
        Measurement::Mean { value: 0.0 }
    );
    let extension = &record.extensions[EXTENSION];
    assert_eq!(extension["planned"], Value::Null);
    let details = extension["connection_details"].as_array().unwrap();
    assert_eq!(details.len(), 4);
    assert_eq!(details[0]["valid"], 1);
    assert_eq!(details[0]["errors"], 1);
    assert_eq!(details[1]["mean_latency_ns"], Value::Null);
    assert_eq!(details[2]["worker"], 1);
    let summary = report.summary();
    assert!(summary.contains("closed loop over 4 connections"));
    assert!(summary.contains("measured window: 1 valid, 0 invalid, 1 failed"));
    assert!(summary.contains("problems: 1 reset"));
}

#[test]
fn records_need_operations() {
    let report = report(Mode::Closed, |_, _, _| {});
    let error = report.record().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "no valid response completed in the measured window"
    );
    // Operations are successes only, so a planned cohort without a valid response has none either, whatever its outcomes.
    let timing = timing(false);
    let report = self::report(Mode::Rate, |_, statistics, connection| {
        let request = Request {
            ordinal: 0,
            planned: 11 * MS,
            sent: 11 * MS,
            eligible: 11 * MS,
        };
        statistics.complete(
            &timing,
            &request,
            12 * MS,
            Some(InvalidKind::Body),
            connection,
        );
        statistics.fail(&timing, &request, 12 * MS, ErrorKind::Reset, connection);
    });
    assert_eq!(report.operations(), 0);
    let error = report.record().unwrap_err();
    assert_eq!(
        error.to_string(),
        "no valid response to a request planned in the measured window"
    );
    // The summary is still written, with empty distributions.
    let summary = report.summary();
    assert!(
        summary.contains("10 planned; 0 valid, 2 invalid, 2 failed"),
        "{summary}"
    );
    assert!(summary.contains("latency: p50 -"), "{summary}");
}

#[test]
fn saturation_and_failures() {
    let timing = timing(false);
    let mut report = report(Mode::Rate, |index, statistics, connection| {
        statistics.busy = if index == 0 { 95 * MS / 10 } else { MS };
        complete(&timing, statistics, connection, 11 * MS, 12 * MS);
        statistics.complete(
            &timing,
            &Request {
                ordinal: 0,
                planned: 11 * MS,
                sent: 11 * MS,
                eligible: 11 * MS,
            },
            12 * MS,
            Some(InvalidKind::Status),
            connection,
        );
        statistics.fail(
            &timing,
            &Request {
                ordinal: 0,
                planned: 5 * MS,
                sent: 5 * MS,
                eligible: 5 * MS,
            },
            6 * MS,
            ErrorKind::Timeout,
            connection,
        );
        statistics.connect_errors = 1;
    });
    report.dispatch_lag_limit = Some(10);
    let saturation = report.saturation();
    assert_eq!(
        saturation,
        [
            "worker 0 was busy for 95.0% of the window",
            "the 99th percentile dispatch lag, 0.05 µs, exceeds 0.01 µs"
        ]
    );
    let none = FailOn::default();
    assert!(report.failures(none).is_empty());
    let all = FailOn {
        invalid: true,
        errors: true,
        saturated: true,
    };
    let failures = report.failures(all);
    assert_eq!(failures[0], "2 invalid responses");
    assert_eq!(
        failures[1],
        "2 failed requests and 2 failed connection attempts"
    );
    assert_eq!(failures.len(), 4);
    assert!(failures[2].starts_with("the client was saturated: worker 0"));
    let record = report.record().unwrap();
    assert_eq!(
        value(&record, "client_saturated"),
        Measurement::Mean { value: 1.0 }
    );
    assert_eq!(
        record.extensions[EXTENSION]["saturation"]["saturated"],
        true
    );
    assert!(report.summary().contains("SATURATED: worker 0"));
    assert!(report.summary().contains("problems: 2 invalid (status)"));
    // The lag limit alone does not apply without a measured lag.
    report.dispatch_lag_limit = Some(1_000_000_000);
    assert_eq!(report.saturation().len(), 1);
}
