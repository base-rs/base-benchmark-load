// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Connections a server starves, as Puma does with more connections than threads: it accepts every keep-alive connection, but serves only as many as it has threads, and the others wait unread until a thread frees up, which with keep-alive never happens. Here [`Server::pool`] starves them by construction, and one worker owns every connection, so the served connections stay open, and the starved ones unanswered, until the run ends.
//!
//! The drain timeout ends the starved requests, so these tests are about it, and assert only what it causes: a starved request waits until the drain deadline, more than the drain timeout after the window's end, whatever the client's speed. The window is a watchdog for the served connections, which would have to stall for all of it to be starved too, so those tests set `--starved-limit` to the whole window.
//!
//! A server can also serve a connection, then stop: [`Server::starving_after`] stops answering some connections, and their reconnections, at a point in the window, and [`Server::rotating`] stops answering connections once the client stops sending, after the window. Their tests assert the starved spans that the point, the timeouts, and the drain timeout cause, with the default `--starved-limit`, a quarter of the window, as the served connections' watchdog.

use super::{CLOSED_WINDOW, count, extension, load, metric, record, server::Server};
use base_benchmark::Direction;
use base_benchmark_load::{ErrorKind, FailOn, Period, Report};
use serde_json::Value;
use std::time::Duration;

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
            "--starved-limit",
            "1",
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
    assert!(drain.errors[ErrorKind::Timeout as usize] >= 2);
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
        report.failures(FailOn {
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
            "--starved-limit",
            "1",
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
    assert!(measured.errors[ErrorKind::Unsent as usize] >= 2 * 99);
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
            "--starved-limit",
            "1",
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
        summary.contains("starved: none of 2 connections; the longest the server left a connection without a response was "),
        "{summary}"
    );
}

/// The window of the runs against [`Server::starving_after`], and its default starved span limit, a quarter of it.
const PARTIAL_WINDOW: &str = "6s";
const PARTIAL_LIMIT_NS: u64 = 1_500_000_000;

/// The response timeout of the runs against [`Server::starving_after`]: shorter than the window, so that no single wait for a response is as long as it, and the slots the server stops answering time out and reconnect in it. It is longer than the limit, so that the served connections' watchdog is the limit: a served connection that timed out would reconnect, and the server never answers a reconnection.
const PARTIAL_TIMEOUT: &str = "2s";

/// Four connections, of which the server stops answering two, and their reconnections, at a point in the window: each was answered in the window, and no request waited as long as the window, but each was starved from the point on, across its timeouts and reconnections, and the rest of the run.
fn assert_starved_after_a_point(report: &Report) {
    let starvation = report.starvation();
    assert_eq!(
        (
            starvation.connections,
            starvation.starved,
            starvation.unanswered,
            starvation.long_span,
            starvation.drain_only
        ),
        (4, 2, 0, 2, 0),
        "{}",
        report.summary()
    );
    assert!(starvation.span_max > PARTIAL_LIMIT_NS);
    // They timed out, and reconnected in the window.
    assert!(report.statistics().connects > 4);
    let record = record(report);
    assert_eq!(metric(&record, "connections_starved"), 2.0);
    assert!(metric(&record, "starved_span_max") > PARTIAL_LIMIT_NS as f64);
    let (starved, served) = details(&record);
    assert_eq!((starved.len(), served.len()), (2, 2));
    for connection in &starved {
        assert!(connection["valid"].as_u64().unwrap() > 0, "{connection}");
        assert!(connection["errors"].as_u64().unwrap() > 0, "{connection}");
        assert!(
            connection["span_window_max_ns"].as_u64().unwrap() > PARTIAL_LIMIT_NS,
            "{connection}"
        );
    }
    let summary = report.summary();
    assert!(
        summary.contains(
            "\nSTARVED: 2 of 4 connections (50.00%) were starved: the server left 2 of them without a response, with a request outstanding or due, for longer than 1.500 s (25% of the 6.000 s window); the longest the server left a connection without a response was "
        ),
        "{summary}"
    );
    assert_eq!(
        report.failures(FailOn {
            starved: true,
            ..Default::default()
        }),
        [report.starved().unwrap()]
    );
}

/// A closed loop against a server that stops answering two of four connections, and their reconnections, a second into the window. A connection always has a request outstanding or to send in the window, so each starved slot's span runs from its last response, at most a second after the window's start unless the client or the server stalls, to the window's end at least: five seconds, longer than the limit, while each of its requests timed out after two.
#[test]
fn a_closed_loop_reports_connections_starved_after_a_point() {
    let server = Server::starving_after(4, Duration::from_secs(1));
    let report = load(
        "closed",
        server.address,
        &[
            "--connections",
            "4",
            "--warmup",
            "0",
            "--duration",
            PARTIAL_WINDOW,
            "--timeout",
            PARTIAL_TIMEOUT,
            "--connection-details",
        ],
    )
    .unwrap();
    assert_starved_after_a_point(&report);
}

/// An open loop against a server that stops answering two of four connections, and their reconnections, two and a half seconds into the window. A starved slot's requests keep falling due every 20 ms, so after each timeout it has a request due, and its span runs from its last response, at most two and a half seconds into the window unless the client or the server stalls, through every timeout and reconnection to its last request's failure, at the drain deadline, a second after the window, so about four and a half seconds. No request waited as long as the window: the longest waited from its planned time, after the point, to the drain deadline.
#[test]
fn an_open_loop_reports_connections_starved_after_a_point() {
    let server = Server::starving_after(4, Duration::from_millis(2500));
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
            PARTIAL_WINDOW,
            "--timeout",
            PARTIAL_TIMEOUT,
            "--drain-timeout",
            "1s",
            "--connection-details",
        ],
    )
    .unwrap();
    assert_starved_after_a_point(&report);
}

/// A closed loop against a server whose one thread serves its four connections in turns of ten requests, waiting for each: fair while the client sends, but once the window ends, it waits on one connection for a request that never comes, and the requests at least two others had in flight are never answered. They time out three seconds after their send, so their spans, from their last response in the window, are longer than the limit, the whole window, but only with the drain: in the window, the rotation answered every connection every forty requests, and the window is the watchdog for a stall of the server's thread. The run is reported as starving them, and why, but `--fail-on starved` keeps it.
#[test]
fn starvation_only_in_the_drain_is_reported_but_spares_the_run() {
    let server = Server::rotating(10);
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
            "--timeout",
            "3s",
            "--drain-timeout",
            "3s",
            "--starved-limit",
            "1",
            "--connection-details",
        ],
    )
    .unwrap();
    let starvation = report.starvation();
    assert!(starvation.starved >= 2, "{}", report.summary());
    assert_eq!(
        (
            starvation.unanswered,
            starvation.long_span,
            starvation.drain_only
        ),
        (0, starvation.starved, starvation.starved),
        "{}",
        report.summary()
    );
    let drain = report.statistics().period(Period::Drain);
    assert!(drain.errors[ErrorKind::Timeout as usize] >= 2);
    let reason = report.starved().unwrap();
    assert!(
        reason.contains(" of them only in the drain, after the window: in the window, the server answered them and left none without a response for that long, so the window measured them fairly; "),
        "{reason}"
    );
    assert!(
        report
            .failures(FailOn {
                starved: true,
                ..Default::default()
            })
            .is_empty()
    );
    let record = record(&report);
    assert_eq!(
        metric(&record, "connections_starved"),
        starvation.starved as f64
    );
    let (starved, _) = details(&record);
    for connection in &starved {
        assert_eq!(connection["drain_only"], true, "{connection}");
        assert!(connection["span_max_ns"].as_u64().unwrap() > 2_000_000_000);
    }
}
