// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::{
    count, extension, load, metric, outcomes, record,
    server::{Action, HELLO, Server},
};
use std::sync::atomic::Ordering;

#[test]
fn every_request_the_server_received_has_one_outcome() {
    for depth in ["1", "8"] {
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
                "200ms",
                "--interval",
                "50ms",
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
        // In-flight requests are completed after the window, not abandoned.
        let total = outcomes(&record, "warmup")
            + outcomes(&record, "measured")
            + outcomes(&record, "drain");
        assert_eq!(total + 1, server.requests.load(Ordering::Relaxed) as u64);
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
    assert!(error.to_string().contains("no valid response"), "{error}");
    assert!(
        report
            .statistics()
            .period(base_benchmark_load::Period::Measured)
            .invalid_total()
            > 0
    );
}
