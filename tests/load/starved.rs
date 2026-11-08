// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Connections a server starves, as Puma does with more connections than threads: it accepts every keep-alive connection, but serves only as many as it has threads, and the others wait unread until a thread frees up, which with keep-alive never happens. Here [`Server::pool`] starves them by construction, and one worker owns every connection, so the served connections stay open, and the starved ones unanswered, until the run ends.
//!
//! The drain timeout ends the starved requests, so these tests are about it, and assert only what it causes: a starved request waits until the drain deadline, more than the drain timeout after the window's end, whatever the client's speed. The window is a watchdog for the served connections, which would have to stall for all of it to be starved too.

use super::{CLOSED_WINDOW, count, extension, load, metric, record, server::Server};
use base_benchmark::Direction;
use base_benchmark_load::Period;
use serde_json::Value;

/// How long after the window the starved requests wait before the drain deadline ends them.
const DRAIN: &str = "1s";
const DRAIN_NS: u64 = 1_000_000_000;

/// The connections' details, split into the starved and the served.
fn details(record: &base_benchmark::ProcessRecord) -> (Vec<Value>, Vec<Value>) {
    extension(record)["connection_details"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .partition(|connection| connection["starved"] == true)
}

/// Every starved connection has no response in the measured period, and its longest wait is the drain deadline's, unanswered.
fn assert_starved(starved: &[Value]) {
    for connection in starved {
        assert_eq!(
            (&connection["valid"], &connection["invalid"]),
            (&Value::from(0), &Value::from(0)),
            "{connection}"
        );
        assert!(
            connection["wait_max_ns"].as_u64().unwrap() > DRAIN_NS,
            "{connection}"
        );
        assert_eq!(connection["wait_max_unanswered"], true, "{connection}");
    }
}

/// A closed loop over four connections to a server with two threads: two are served throughout the window, and two are never answered. The requests in flight on them at the window's end are drained until the deadline, not abandoned at the window's end, and end as timeouts, so the record and the summary say that half the connections were starved, though the service time of the served connections is healthy.
#[test]
fn a_closed_loop_reports_the_connections_the_server_starves() {
    let server = Server::pool(2);
    let report = load(
        "closed",
        server.address,
        &[
            "--connections",
            "4",
            "--warmup",
            "0",
            "--duration",
            CLOSED_WINDOW,
            "--drain-timeout",
            DRAIN,
            "--connection-details",
        ],
    )
    .unwrap();
    let starvation = report.starvation();
    assert_eq!(
        (
            starvation.connections,
            starvation.starved,
            starvation.unanswered
        ),
        (4, 2, 2)
    );
    assert!(starvation.wait_max > DRAIN_NS);
    assert!(starvation.wait_unanswered);
    // Each starved connection's one request in flight timed out at the drain deadline, after the window; the served connections' requests in flight at its end are drained too.
    let drain = report.statistics().period(Period::Drain);
    assert!(drain.errors[base_benchmark_load::ErrorKind::Timeout as usize] >= 2);
    let record = record(&report);
    assert_eq!(metric(&record, "connections_starved"), 2.0);
    assert_eq!(metric(&record, "connections_starved_fraction"), 0.5);
    assert!(metric(&record, "response_wait_max") > DRAIN_NS as f64);
    // The server starved them, so starvation ranks servers, and fewer and shorter is better.
    for name in [
        "connections_starved",
        "connections_starved_fraction",
        "response_wait_max",
    ] {
        assert_eq!(
            record.samples[0].metric(name).unwrap().direction,
            Direction::LowerIsBetter,
            "{name}"
        );
    }
    let starvation = &extension(&record)["starvation"];
    assert_eq!(
        (&starvation["starved"], &starvation["unanswered"]),
        (&Value::from(2), &Value::from(2))
    );
    assert_eq!(starvation["wait_max_unanswered"], true);
    let (starved, served) = details(&record);
    assert_eq!((starved.len(), served.len()), (2, 2));
    assert_starved(&starved);
    for connection in &served {
        assert!(connection["valid"].as_u64().unwrap() > 0, "{connection}");
    }
    // The service time holds the served connections' valid responses only, however healthy, so the summary says, before any distribution, that half the connections were starved.
    assert_eq!(
        extension(&record)["histograms"]["service_time"]["count"],
        count(&record, "responses_valid")
    );
    let summary = report.summary();
    let line = summary
        .lines()
        .find(|line| line.starts_with("STARVED: "))
        .unwrap_or_else(|| panic!("{summary}"));
    assert!(
        line.starts_with(
            "STARVED: 2 of 4 connections (50.00%) were starved: the server answered none of the requests of 2 of them in the window"
        ),
        "{line}"
    );
    assert!(
        line.contains(", and its request was never answered"),
        "{line}"
    );
    assert!(summary.find("STARVED: ").unwrap() < summary.find("service_time: ").unwrap());
    assert!(
        summary.contains("in flight at the window's end, then drained: "),
        "{summary}"
    );
    // Starvation is the server's doing, never the client's saturation.
    assert!(
        report
            .saturation()
            .iter()
            .all(|reason| !reason.contains("starved"))
    );
    assert_eq!(
        report.failures(base_benchmark_load::FailOn {
            starved: true,
            ..Default::default()
        }),
        [report.starved().unwrap()]
    );
}

/// An open loop over four connections to a server with two threads: on each starved connection the first request is sent and never answered, and every later one waits unsent behind it, until the drain deadline. None of their requests planned in the window is answered, so they are starved, and the requests they hold back are the server's doing.
#[test]
fn an_open_loop_reports_the_connections_the_server_starves() {
    let server = Server::pool(2);
    let report = load(
        "rate",
        server.address,
        &[
            "--rate",
            "200",
            "--connections",
            "4",
            "--warmup",
            "0",
            "--duration",
            CLOSED_WINDOW,
            "--drain-timeout",
            DRAIN,
            "--connection-details",
        ],
    )
    .unwrap();
    let starvation = report.starvation();
    assert_eq!(
        (
            starvation.connections,
            starvation.starved,
            starvation.unanswered
        ),
        (4, 2, 2)
    );
    assert!(starvation.wait_unanswered);
    let record = record(&report);
    assert_eq!(metric(&record, "connections_starved"), 2.0);
    let (starved, served) = details(&record);
    assert_eq!((starved.len(), served.len()), (2, 2));
    assert_starved(&starved);
    for connection in &starved {
        // A hundred of the window's 400 requests are each connection's.
        assert_eq!(connection["planned"], 100, "{connection}");
        assert_eq!(connection["errors"], 1, "the one request sent timed out");
    }
    // Each starved connection's 99 requests never sent are failed as unsent, at least.
    let measured = report.statistics().period(Period::Measured);
    assert!(measured.errors[base_benchmark_load::ErrorKind::Unsent as usize] >= 2 * 99);
    // Each starved connection sent at most its first request in the window, so at least 198 of the window's requests were not sent in it. Those the worker reached in the window, as it does when it keeps its schedule, the connections held back, and the reason says that connections were starved, whichever side it names.
    assert!(report.shortfall() >= 2 * 99, "{}", report.shortfall());
    let held_back = report.held_back();
    assert_eq!(
        held_back.is_some(),
        report.connection_shortfall() > 0,
        "{held_back:?}"
    );
    if let Some(held_back) = held_back {
        assert!(
            held_back.ends_with(
                "; 2 of the 4 connections were starved, with requests the server left waiting"
            ),
            "{held_back}"
        );
    }
    assert!(
        report
            .summary()
            .contains("STARVED: 2 of 4 connections (50.00%) were starved: the server answered none of the requests of 2 of them in the window")
    );
}

/// A server with a thread for every connection starves none, and the summary says so, with the longest wait. The preflight connection is closed after its response, so it leaves both threads to the load.
#[test]
fn a_server_that_serves_every_connection_starves_none() {
    let server = Server::pool(2);
    let report = load(
        "closed",
        server.address,
        &[
            "--connections",
            "2",
            "--warmup",
            "0",
            "--duration",
            CLOSED_WINDOW,
        ],
    )
    .unwrap();
    assert_eq!(report.starvation().starved, 0);
    assert_eq!(report.starved(), None);
    let record = record(&report);
    assert_eq!(metric(&record, "connections_starved"), 0.0);
    assert_eq!(extension(&record)["starvation"]["reason"], Value::Null);
    let summary = report.summary();
    assert!(
        summary.contains("starved: none of 2 connections; the longest wait for a response was "),
        "{summary}"
    );
}
