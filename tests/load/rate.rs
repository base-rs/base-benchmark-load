// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The open loop, porting the fixed-rate client's `validate.py`: exact counts across workers, rejection of a corrupted response, and a stalled server whose delay stays visible.

use super::{
    MS, count, extension, load, measured, metric, outcomes, record, requests,
    server::{Action, HELLO, Server},
};
use std::{sync::atomic::Ordering, time::Duration};

#[test]
fn every_planned_request_is_validated_exactly_once() {
    let server = Server::hello();
    let report = load(
        "rate",
        server.address,
        &[
            "--rate",
            "2000",
            "--connections",
            "4",
            "--workers",
            "2",
            "--warmup",
            "100ms",
            "--duration",
            "500ms",
        ],
    )
    .unwrap();
    let record = record(&report);
    assert_eq!(record.workload, "base-benchmark-load/http1");
    assert_eq!(record.samples[0].operations, 1000);
    assert_eq!(count(&record, "responses_valid"), 1000);
    assert_eq!(count(&record, "responses_invalid"), 0);
    assert_eq!(count(&record, "errors"), 0);
    assert_eq!(count(&record, "connect_errors"), 0);
    assert_eq!(extension(&record)["periods"]["warmup"]["valid"], 200);
    assert_eq!(outcomes(&record, "drain"), 0);
    assert_eq!(extension(&record)["planned"], 1000);
    // The preflight request, the warmup, and the measured cohort, and nothing else.
    assert_eq!(extension(&record)["preflight_requests"], 1);
    assert_eq!(requests(&record), 1 + 200 + 1000);
    assert_eq!(server.requests.load(Ordering::Relaxed), 1 + 200 + 1000);
    let signature = format!("http1 status 200 body 13:{}", digest_of_hello());
    assert_eq!(record.signature.as_deref(), Some(signature.as_str()));
    // Every distribution counts the cohort, and quantiles are ordered.
    for name in [
        "latency",
        "round_trip",
        "send_delay",
        "dispatch_lag",
        "schedule_lag",
    ] {
        assert_eq!(
            extension(&record)["histograms"][name]["count"],
            1000,
            "{name}"
        );
        assert!(metric(&record, &format!("{name}_p50")) <= metric(&record, &format!("{name}_p99")));
        assert!(metric(&record, &format!("{name}_p99")) <= metric(&record, &format!("{name}_max")));
    }
    // Latency includes the send delay, so it is never below the round trip.
    assert!(metric(&record, "latency_max") >= metric(&record, "round_trip_max"));
    let workers = extension(&record)["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 2);
    let intervals = extension(&record)["intervals"].as_array().unwrap();
    assert_eq!(intervals.len(), 1);
    assert_eq!(intervals[0]["latency_count"], 1000);
    let fairness = &extension(&record)["fairness"]["connection_completions"];
    assert_eq!(
        (fairness["min"].as_f64(), fairness["max"].as_f64()),
        (Some(250.0), Some(250.0))
    );
    assert_eq!(fairness["jain"].as_f64(), Some(1.0));
    let summary = report.summary();
    assert!(
        summary.contains("1000 planned; 1000 valid, 0 invalid, 0 failed"),
        "{summary}"
    );
}

fn digest_of_hello() -> String {
    // FNV-1a of "Hello, world!".
    let hash = b"Hello, world!"
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{hash:016x}")
}

/// `validate.py`'s stall case: one connection at 1,000 requests per second, and a 40 ms stall before the first response. The requests planned during the stall wait for it, so the stall stays in their latency from the planned time, whatever the client's speed.
#[test]
fn a_server_stall_stays_visible_in_open_loop_latency() {
    let server = Server::start(|arrival| match (arrival.connection, arrival.request) {
        (1, 0) => Action::Delay(Duration::from_millis(40), HELLO.to_vec()),
        _ => Action::Respond(HELLO.to_vec()),
    });
    let report = load(
        "rate",
        server.address,
        &[
            "--rate",
            "1000",
            "--connections",
            "1",
            "--warmup",
            "0",
            "--duration",
            "1s",
        ],
    )
    .unwrap();
    let record = record(&report);
    assert_eq!(count(&record, "responses_valid"), 1000);
    assert_eq!(count(&record, "errors"), 0);
    assert!(metric(&record, "latency_max") >= 40.0 * MS);
    // The request planned 1 ms after the stalled one could only be sent once it completed.
    assert!(metric(&record, "send_delay_max") >= 39.0 * MS);
    // Request k, planned k ms after the stalled one, waits at least 40 - k ms. The 99th percentile of 1,000 is the eleventh largest, so it is at least 30 ms.
    assert!(metric(&record, "latency_p99") >= 30.0 * MS);
    let intervals = extension(&record)["intervals"].as_array().unwrap();
    assert!(intervals[0]["latency_max_ns"].as_f64().unwrap() >= 40.0 * MS);
}

/// A run whose window ends with requests it planned in it still unsent did not offer its rate in the window. Here the server stalls the response to the request planned 150 ms into a 200 ms window for 100 ms, so the 49 requests planned after it on the one connection can only be sent after the window, however fast the client is. Those the worker reached in the window, as it does when it keeps its schedule, the server held back, which does not saturate the client; any it reached only after the window, it held back itself. Without a warmup, every request sent in the window was reached in it, so the two parts add up to the shortfall.
#[test]
fn requests_planned_in_the_window_but_sent_after_it_are_a_shortfall() {
    let server = Server::start(|arrival| match (arrival.connection, arrival.request) {
        (1, 150) => Action::Delay(Duration::from_millis(100), HELLO.to_vec()),
        _ => Action::Respond(HELLO.to_vec()),
    });
    let report = load(
        "rate",
        server.address,
        &[
            "--rate",
            "1000",
            "--connections",
            "1",
            "--warmup",
            "0",
            "--duration",
            "200ms",
        ],
    )
    .unwrap();
    let record = record(&report);
    assert_eq!(count(&record, "responses_valid"), 200);
    let (shortfall, schedule, connection) = (
        report.shortfall(),
        report.schedule_shortfall(),
        report.connection_shortfall(),
    );
    assert!(shortfall >= 49, "{shortfall}");
    assert_eq!(schedule + connection, shortfall);
    assert_eq!(count(&record, "sent_in_window_shortfall"), shortfall);
    assert_eq!(count(&record, "schedule_shortfall"), schedule);
    assert_eq!(count(&record, "connection_shortfall"), connection);
    assert_eq!(count(&record, "sent_in_window"), 200 - shortfall);
    // Any part is over the limit of one in a thousand, and judged by which side held the requests back.
    let held_back = report.held_back();
    assert_eq!(held_back.is_some(), connection > 0, "{held_back:?}");
    let reasons = report.saturation();
    assert_eq!(
        reasons.iter().any(
            |reason| reason.contains("fewer requests in the window than the 200 planned in it")
        ),
        schedule > 0,
        "{reasons:?}"
    );
    assert_eq!(
        metric(&record, "client_saturated"),
        f64::from(u8::from(!reasons.is_empty()))
    );
    assert_eq!(report.summary().contains(" held back "), connection > 0);
}

/// With pipelining, requests sent behind the stalled one are on the wire and still wait for it.
#[test]
fn a_pipelined_stall_is_retained() {
    let server = Server::start(|arrival| match (arrival.connection, arrival.request) {
        (1, 0) => Action::Delay(Duration::from_millis(40), HELLO.to_vec()),
        _ => Action::Respond(HELLO.to_vec()),
    });
    let report = load(
        "rate",
        server.address,
        &[
            "--rate",
            "1000",
            "--connections",
            "1",
            "--depth",
            "4",
            "--warmup",
            "0",
            "--duration",
            "1s",
        ],
    )
    .unwrap();
    let record = record(&report);
    assert_eq!(count(&record, "responses_valid"), 1000);
    assert!(metric(&record, "latency_max") >= 40.0 * MS);
    assert!(metric(&record, "latency_p99") >= 30.0 * MS);
    // The fifth request could not be sent until the first completed.
    assert!(metric(&record, "send_delay_max") >= 36.0 * MS);
}

/// `validate.py`'s corruption case: the preflight's body becomes the expected body, and a later response that differs is invalid, not an error and not a success. With no success, there are no operations, and so no record.
#[test]
fn a_corrupted_response_is_invalid() {
    let server = Server::start(|arrival| {
        let mut response = HELLO.to_vec();
        if arrival.connection > 0 {
            *response.last_mut().unwrap() = b'?';
        }
        Action::Respond(response)
    });
    let report = load(
        "rate",
        server.address,
        &[
            "--rate",
            "1000",
            "--connections",
            "1",
            "--warmup",
            "0",
            "--duration",
            "200ms",
        ],
    )
    .unwrap();
    let outcomes = measured(&report);
    assert_eq!(outcomes.valid, 0);
    assert_eq!(outcomes.invalid_total(), 200);
    assert_eq!(
        outcomes.invalid[base_benchmark_load::InvalidKind::Body as usize],
        200
    );
    assert_eq!(outcomes.error_total(), 0);
    assert_eq!(report.statistics().distributions.latency.count(), 0);
    assert!(report.summary().contains("200 invalid (body)"));
    assert!(report.record().is_err());
}

/// The address differs between equal runs, as when `base-benchmark` starts a server on a new port for every sample, so it is never a parameter, nor is the Host header that names it by default.
#[test]
fn parameters_do_not_depend_on_the_address() {
    // Both servers live throughout, so their ports differ.
    let servers = [Server::hello(), Server::hello()];
    let parameters = |server: &Server| {
        let report = load(
            "rate",
            server.address,
            &[
                "--rate",
                "1000",
                "--connections",
                "1",
                "--warmup",
                "0",
                "--duration",
                "10ms",
            ],
        )
        .unwrap();
        let record = record(&report);
        assert_eq!(
            extension(&record)["context"]["address"],
            server.address.to_string()
        );
        record.parameters
    };
    assert_ne!(servers[0].address, servers[1].address);
    let first = parameters(&servers[0]);
    assert_eq!(first, parameters(&servers[1]));
    assert_eq!(first["request"]["host"], "default");
}
