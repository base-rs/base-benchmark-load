// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Connections a server starves, as Puma does with more connections than threads: it accepts every keep-alive connection, but serves only as many as it has threads, and the others wait unread until a thread frees up, which with keep-alive never happens. Here [`Server::pool`] starves them by construction, and one worker owns every connection, so the served connections stay open, and the starved ones unanswered, until the run ends.
//!
//! The drain timeout ends the starved requests, so these tests are about it, and assert only what it causes: a starved request waits until the drain deadline, more than the drain timeout after the window's end, whatever the client's speed. The window is a watchdog for the served connections, which would have to stall for all of it to be starved too, so those tests set `--starved-limit` to the whole window.
//!
//! A server can also serve a connection, then stop: [`Server::starving_after`] stops answering some connections, and their reconnections, after a number of responses, and [`Server::relaying`] answers each request only when the next arrives, so that once the client stops sending, after the window, the last request it sent is never answered. The points are events, not times, so the tests assert what those events cause, however slowly the host runs: a span from a connection's last response to the end of the run, and in the drain, a request held until the drain deadline. The spans of the connections the server answers are near a round trip, and the default `--starved-limit`, a quarter of the window, is their watchdog; a fair server is not flagged in the unit tests, with explicit times, rather than here.

use super::{CLOSED_WINDOW, count, extension, load, metric, record, server::Server};
use base_benchmark::Direction;
use base_benchmark_load::{ErrorKind, FailOn, Period, Report};
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

/// The window of the runs against [`Server::starving_after`] and [`Server::relaying`], and its default starved span limit, a quarter of it: the watchdog of the served connections' spans in the window, which the client or the server would have to stall for a second and a half to exceed.
const PARTIAL_WINDOW: &str = "6s";
const PARTIAL_LIMIT_NS: u64 = 1_500_000_000;

/// The response timeout of the runs against [`Server::starving_after`]: shorter than the window, so that no single wait for a response is as long as it, and the slots the server stops answering time out and reconnect.
const PARTIAL_TIMEOUT: &str = "2s";
const PARTIAL_TIMEOUT_NS: u64 = 2_000_000_000;

/// The responses [`Server::starving_after`] gives a connection it stops answering: a few, so that each was served in the window, then starved.
const ANSWERED: usize = 5;

/// Four connections, of which the server stops answering two after their fifth response, and every reconnection. Each of those two was answered in the window, and no request waited as long as the window, though one waited at least the timeout, but each was starved from its fifth response on, across its timeouts and reconnections, to the end of the run: since its first five responses come early in the window, unless the client or the server stalls for three quarters of it before them, its span in the window is longer than the limit. A stall of the client or the server longer than `--timeout` can also fail a served connection's request, and the server never answers its reconnection, so it can be starved too: the counts are only lower bounds, but the server answered every connection, so none went unanswered.
fn assert_starved_after_a_point(report: &Report) {
    let starvation = report.starvation();
    let summary = report.summary();
    assert_eq!(
        (starvation.connections, starvation.unanswered),
        (4, 0),
        "{summary}"
    );
    assert!(starvation.starved >= 2, "{summary}");
    assert!(starvation.long_span >= 2, "{summary}");
    assert!(starvation.in_window() >= 2, "{summary}");
    assert!(starvation.span_max > PARTIAL_LIMIT_NS, "{summary}");
    let record = record(report);
    assert!(metric(&record, "connections_starved") >= 2.0);
    assert!(metric(&record, "starved_span_max") > PARTIAL_LIMIT_NS as f64);
    // The connections the server stopped answering are those it answered exactly five times: a served connection is answered until the run ends, or until a stall fails its request, and then it is starved too.
    let (starved, _) = details(&record);
    let stopped: Vec<&Value> = starved
        .iter()
        .filter(|connection| connection["valid"] == ANSWERED)
        .collect();
    assert!(stopped.len() >= 2, "{starved:?}");
    // The request after the fifth response is never answered, so it waited at least until it timed out, whether that was in the window or, after a stall, in the drain.
    for connection in stopped {
        assert!(
            connection["wait_max_ns"].as_u64().unwrap() >= PARTIAL_TIMEOUT_NS,
            "{connection}"
        );
        assert!(
            connection["span_window_max_ns"].as_u64().unwrap() > PARTIAL_LIMIT_NS,
            "{connection}"
        );
    }
    assert!(
        summary.contains(" without a response, with a request outstanding or due, for longer than 1.500 s (25% of the 6.000 s window); "),
        "{summary}"
    );
    // Before any distribution: the closed loop's first is the service time, the open loop's the latency.
    let distribution = summary
        .find("\nservice_time: ")
        .or_else(|| summary.find("\nlatency: "))
        .unwrap();
    assert!(
        summary.find("\nSTARVED: ").unwrap() < distribution,
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

/// A closed loop against a server that stops answering two of four connections after their fifth response, and every reconnection. A connection always has a request outstanding or to send in the window, so each starved slot's span runs from its fifth response to the window's end at least, while each of its requests timed out after two seconds.
#[test]
fn a_closed_loop_reports_connections_starved_after_a_point() {
    let server = Server::starving_after(4, ANSWERED);
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

/// An open loop against a server that stops answering two of four connections after their fifth response, and every reconnection. A starved slot's requests keep falling due every 20 ms, so after each timeout it has a request due, and its span runs from its fifth response through every timeout and reconnection to its last request's failure, at the drain deadline, a second after the window. No request waited as long as the window: the longest waited from its planned time, after the point, to the drain deadline.
#[test]
fn an_open_loop_reports_connections_starved_after_a_point() {
    let server = Server::starving_after(4, ANSWERED);
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

/// How long after the window [`Server::relaying`] holds the last request before the drain deadline ends it: longer than the limit, so that the drain alone makes its span longer than the limit.
const RELAY_DRAIN: &str = "2s";
const RELAY_DRAIN_NS: u64 = 2_000_000_000;

/// A closed loop over four connections against a server that answers each request when the next arrives. While the client sends, every request is answered by the next arrival, so every connection is answered in the window, within the limit. Once the window ends, the client sends nothing more, so exactly one request, the last to arrive, is never answered: it waits until the drain deadline, and its connection's span, from its last response in the window, is longer than the limit only with the drain. The run is reported as starving it, and why, but `--fail-on starved` keeps it.
#[test]
fn starvation_only_in_the_drain_is_reported_but_spares_the_run() {
    let server = Server::relaying();
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
            "--drain-timeout",
            RELAY_DRAIN,
            "--connection-details",
        ],
    )
    .unwrap();
    let starvation = report.starvation();
    assert_eq!(
        (
            starvation.connections,
            starvation.starved,
            starvation.unanswered,
            starvation.long_span,
            starvation.drain_only
        ),
        (4, 1, 0, 1, 1),
        "{}",
        report.summary()
    );
    assert!(starvation.span_max > RELAY_DRAIN_NS);
    assert!(starvation.wait_unanswered);
    // The held request is the only one never answered, and the drain deadline timed it out.
    let drain = report.statistics().period(Period::Drain);
    assert_eq!(drain.errors[ErrorKind::Timeout as usize], 1);
    let reason = report.starved().unwrap();
    assert!(
        reason.contains("; 1 of them only in the drain, after the window: in the window, the server answered them, and their starved spans were within the limit; "),
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
    assert_eq!(metric(&record, "connections_starved"), 1.0);
    let (starved, _) = details(&record);
    assert_eq!(starved.len(), 1);
    let connection = &starved[0];
    assert_eq!(connection["drain_only"], true, "{connection}");
    assert!(connection["valid"].as_u64().unwrap() > 0, "{connection}");
    assert!(
        connection["span_max_ns"].as_u64().unwrap() > RELAY_DRAIN_NS,
        "{connection}"
    );
    assert_eq!(connection["wait_max_unanswered"], true, "{connection}");
}
