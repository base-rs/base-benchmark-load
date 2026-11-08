// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::{
    CLOSED_WINDOW, count, extension, load, metric, record, requests,
    server::{Action, HELLO, Server},
};
use base_benchmark_load::Period;
use std::sync::atomic::Ordering;

/// One closed loop at `depth`, over a window long enough to be only a watchdog. Its counts are compared with the requests the server received, never with how many fit in the window.
fn every_request_has_one_outcome(depth: &str) {
    let server = Server::hello();
    let report = load(
        "closed",
        server.address,
        &[
            "--connections",
            "3",
            "--depth",
            depth,
            "--workers",
            "2",
            "--warmup",
            "50ms",
            "--duration",
            CLOSED_WINDOW,
            "--interval",
            "500ms",
        ],
    )
    .unwrap();
    let record = record(&report);
    let valid = count(&record, "responses_valid");
    assert!(valid > 0);
    assert_eq!(record.samples[0].operations, valid);
    assert_eq!(
        count(&record, "errors") + count(&record, "responses_invalid"),
        0
    );
    // In-flight requests are completed after the window, not abandoned, so every request the server received has an outcome.
    assert_eq!(
        requests(&record),
        server.requests.load(Ordering::Relaxed) as u64
    );
    assert_eq!(
        extension(&record)["periods"]["drain"]["errors"]["timeout"],
        0
    );
    // Closed loops report service time, never open-loop latency.
    assert!(record.samples[0].metric("latency_p99").is_none());
    assert!(metric(&record, "service_time_p50") <= metric(&record, "service_time_max"));
    assert_eq!(
        extension(&record)["histograms"]["service_time"]["count"],
        valid
    );
    assert_eq!(extension(&record)["intervals"].as_array().unwrap().len(), 4);
    assert_eq!(record.parameters["mode"], "closed");
    assert_eq!(record.parameters["arrivals"], serde_json::Value::Null);
    assert!(report.summary().contains("closed loop over 3 connections"));
}

#[test]
fn every_request_the_server_received_has_one_outcome() {
    // Both depths at once, since each waits for its window.
    std::thread::scope(|scope| {
        for depth in ["1", "8"] {
            scope.spawn(move || every_request_has_one_outcome(depth));
        }
    });
}

#[test]
fn a_closed_loop_without_a_valid_response_has_no_record() {
    let server = Server::start(|arrival| {
        let mut response = HELLO.to_vec();
        if arrival.connection > 0 {
            *response.last_mut().unwrap() = b'?';
        }
        Action::Respond(response)
    });
    let report = load(
        "closed",
        server.address,
        &["--connections", "1", "--warmup", "0", "--duration", "50ms"],
    )
    .unwrap();
    let error = report.record().unwrap_err();
    assert_eq!(
        error.to_string(),
        "no valid response completed in the measured window"
    );
    // The first request is answered, in the window or in the drain after it, however slowly; every answer is invalid.
    let statistics = report.statistics();
    let invalid: u64 = Period::ALL
        .iter()
        .map(|period| statistics.period(*period).invalid_total())
        .sum();
    assert!(invalid > 0);
    assert_eq!(statistics.finished(), invalid);
}
