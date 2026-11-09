// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Connections a server starves, as Puma does with more connections than threads: it accepts every keep-alive connection, but serves only as many as it has threads, and the others wait unread until a thread frees up, which with keep-alive never happens. Here [`Server::pool`] starves them by construction, and one worker owns every connection, so the served connections stay open, and the starved ones unanswered, until the run ends.
//!
//! The drain timeout ends the starved requests, so these tests are about it, and assert only what it causes: a starved request waits until the drain deadline, more than the drain timeout after the window's end, whatever the client's speed. Those tests set `--starved-limit` to the whole window, and a span's part in the window is never longer than the window, so no span can exceed the limit in the window: a stall can starve a served connection in the window only by covering all of the window, which is its watchdog, leaving it unanswered. A stall across the window's end can still lengthen a served connection's span, open at the end, past the limit with the drain, which starves it only in the drain, so those tests assert exact counts of the connections unanswered and starved in the window, and only lower bounds of the starved.
//!
//! A server can also serve a connection, then stop: [`Server::starving_after`] stops answering some connections, and their reconnections, after a number of responses, and [`Server::relaying`] answers each request only when the next arrives, so that once the client stops sending, after the window, the last request it sent is never answered. The points are events, not times, so the tests assert what those events cause, however slowly the host runs: a span from a connection's last response to the end of the run, and in the drain, a request held until the drain deadline. Against [`Server::starving_after`], the spans of the connections the server answers are near a round trip, and the default `--starved-limit`, a quarter of the window, is their watchdog, so the counts are lower bounds; against [`Server::relaying`], the limit is the whole window, as against [`Server::pool`]. A fair server is not flagged in the unit tests, with explicit times, and here only in the window.

use super::{CLOSED_WINDOW, count, extension, load, metric, record, server::Server};
use base_benchmark::Direction;
use base_benchmark_load::{ErrorKind, FailOn, Period, Report};
use serde_json::Value;

/// How long after the window the starved requests wait before the drain deadline ends them.
const DRAIN: &str = "1s";
const DRAIN_NS: u64 = 1_000_000_000;

/// The connections' details.
fn connections(record: &base_benchmark::ProcessRecord) -> Vec<Value> {
    extension(record)["connection_details"]
        .as_array()
        .unwrap()
        .clone()
}

/// The connections' details, split into the starved and the served.
fn details(record: &base_benchmark::ProcessRecord) -> (Vec<Value>, Vec<Value>) {
    connections(record)
        .into_iter()
        .partition(|connection| connection["starved"] == true)
}

/// The connections' details, split into those the server answered none of the measured period's requests of, and the others.
fn unanswered(record: &base_benchmark::ProcessRecord) -> (Vec<Value>, Vec<Value>) {
    connections(record).into_iter().partition(|connection| {
        (&connection["valid"], &connection["invalid"]) == (&Value::from(0), &Value::from(0))
    })
}

/// Every connection the server never answered is starved in the window, and its longest wait is the drain deadline's, unanswered.
fn assert_starved(unanswered: &[Value]) {
    for connection in unanswered {
        assert_eq!(
            (&connection["starved"], &connection["drain_only"]),
            (&Value::from(true), &Value::from(false)),
            "{connection}"
        );
        assert!(
            connection["wait_max_ns"].as_u64().unwrap() > DRAIN_NS,
            "{connection}"
        );
        assert_eq!(connection["wait_max_unanswered"], true, "{connection}");
    }
}

/// Every connection the server answered was served, and starved, if at all, only in the drain, by a stall across the window's end.
fn assert_served(answered: &[Value]) {
    for connection in answered {
        assert!(connection["valid"].as_u64().unwrap() > 0, "{connection}");
        assert_eq!(
            connection["drain_only"], connection["starved"],
            "{connection}"
        );
    }
}

/// A closed loop over four connections to a server with two threads: two are served throughout the window, and two are never answered. The requests in flight on them at the window's end are drained until the deadline, not abandoned at the window's end, and end as timeouts, so the record and the summary say that at least half the connections were starved, though the service time of the served connections is healthy. Exactly the two never answered are starved in the window; a stall across the window's end can starve the served ones too, only in the drain.
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
    let summary = report.summary();
    assert_eq!(
        (
            starvation.connections,
            starvation.unanswered,
            starvation.in_window()
        ),
        (4, 2, 2),
        "{summary}"
    );
    assert!(starvation.starved >= 2, "{summary}");
    assert!(starvation.wait_max > DRAIN_NS);
    assert!(starvation.wait_unanswered);
    // Each starved connection's one request in flight timed out at the drain deadline, after the window; the served connections' requests in flight at its end are drained too.
    let drain = report.statistics().period(Period::Drain);
    assert!(drain.errors[ErrorKind::Timeout as usize] >= 2);
    let record = record(&report);
    assert_eq!(
        metric(&record, "connections_starved"),
        starvation.starved as f64
    );
    assert_eq!(
        metric(&record, "connections_starved_fraction"),
        starvation.fraction()
    );
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
    let extension_starvation = &extension(&record)["starvation"];
    assert_eq!(
        (
            &extension_starvation["starved"],
            &extension_starvation["unanswered"]
        ),
        (&Value::from(starvation.starved), &Value::from(2))
    );
    assert_eq!(extension_starvation["wait_max_unanswered"], true);
    let (never, answered) = unanswered(&record);
    assert_eq!((never.len(), answered.len()), (2, 2));
    assert_starved(&never);
    assert_served(&answered);
    assert_eq!(details(&record).0.len(), starvation.starved);
    // The service time holds the served connections' valid responses only, however healthy, so the summary says, before any distribution, that at least half the connections were starved.
    assert_eq!(
        extension(&record)["histograms"]["service_time"]["count"],
        count(&record, "responses_valid")
    );
    let line = summary
        .lines()
        .find(|line| line.starts_with("STARVED: "))
        .unwrap_or_else(|| panic!("{summary}"));
    assert!(
        line.starts_with(&format!(
            "STARVED: {} of 4 connections ({:.2}%) were starved: the server answered none of the requests of 2 of them in the window",
            starvation.starved,
            starvation.fraction() * 100.0
        )),
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

/// An open loop over four connections to a server with two threads: on each starved connection the first request is sent and never answered, and every later one waits unsent behind it, until the drain deadline. None of their requests planned in the window is answered, so they are starved, and the requests they hold back are the server's doing. As in the closed loop, a stall across the window's end can starve the served connections too, only in the drain.
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
    let summary = report.summary();
    assert_eq!(
        (
            starvation.connections,
            starvation.unanswered,
            starvation.in_window()
        ),
        (4, 2, 2),
        "{summary}"
    );
    assert!(starvation.starved >= 2, "{summary}");
    assert!(starvation.wait_unanswered);
    let record = record(&report);
    assert_eq!(
        metric(&record, "connections_starved"),
        starvation.starved as f64
    );
    let (never, answered) = unanswered(&record);
    assert_eq!((never.len(), answered.len()), (2, 2));
    assert_starved(&never);
    assert_served(&answered);
    for connection in &never {
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
            held_back.ends_with(&format!(
                "; {} of the 4 connections were starved, with requests the server left waiting",
                starvation.starved
            )),
            "{held_back}"
        );
    }
    assert!(
        summary.contains(&format!(
            "STARVED: {} of 4 connections ({:.2}%) were starved: the server answered none of the requests of 2 of them in the window",
            starvation.starved,
            starvation.fraction() * 100.0
        )),
        "{summary}"
    );
}

/// A server with a thread for every connection starves none in the window, and the summary says so, with the longest wait. The preflight connection is closed after its response, so it leaves both threads to the load. A stall across the window's end can lengthen a connection's span, open at the end, past the limit with the drain, and then the run reports it as starved only in the drain, and still passes `--fail-on starved`.
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
            "--connection-details",
        ],
    )
    .unwrap();
    let starvation = report.starvation();
    let summary = report.summary();
    assert_eq!(
        (
            starvation.connections,
            starvation.unanswered,
            starvation.in_window()
        ),
        (2, 0, 0),
        "{summary}"
    );
    assert_eq!(starvation.drain_only, starvation.starved, "{summary}");
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
    let (never, answered) = unanswered(&record);
    assert_eq!((never.len(), answered.len()), (0, 2));
    assert_served(&answered);
    if starvation.starved == 0 {
        assert_eq!(report.starved(), None);
        assert_eq!(extension(&record)["starvation"]["reason"], Value::Null);
        assert!(
            summary.contains("starved: none of 2 connections; the longest the server left a connection without a response was "),
            "{summary}"
        );
    } else {
        let reason = report.starved().unwrap();
        assert!(
            reason.contains(&format!(
                "; {} of them only in the drain, after the window: ",
                starvation.drain_only
            )),
            "{reason}"
        );
    }
}

/// The window of the runs against [`Server::starving_after`], and its default starved span limit, a quarter of it: the watchdog of the served connections' spans in the window, which the client or the server would have to stall for a second and a half to exceed.
const PARTIAL_WINDOW: &str = "6s";
const PARTIAL_LIMIT_NS: u64 = 1_500_000_000;

/// The response timeout of the runs against [`Server::starving_after`]: shorter than the window, so that in the closed loop, where each request waits from its send until its response or the timeout, no wait is as long as the window unless a stall lengthens it, and the slots the server stops answering time out and reconnect.
const PARTIAL_TIMEOUT: &str = "2s";
const PARTIAL_TIMEOUT_NS: u64 = 2_000_000_000;

/// The responses [`Server::starving_after`] gives a connection it stops answering: a few, so that each was served in the window, then starved.
const ANSWERED: usize = 5;

/// Four connections, of which the server stops answering two after their fifth response, and every reconnection. Each of those two was answered in the window, and one of its requests waited at least the timeout, but each was starved from its fifth response on, across its timeouts and reconnections, to the end of the run: since its first five responses come early in the window, unless the client or the server stalls for three quarters of it before them, its span in the window is longer than the limit. A stall of the client or the server longer than `--timeout` can also fail a served connection's request, and the server never answers its reconnection, so it can be starved too: the counts are only lower bounds, but the server answered every connection, so none went unanswered.
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

/// A closed loop against a server that stops answering two of four connections after their fifth response, and every reconnection. A connection always has a request outstanding or to send in the window, so each starved slot's span runs from its fifth response to the window's end at least, while each of its requests timed out after two seconds. Only this test shows that the span finds starvation where no request waited as long as the window: each request waited from its send until its response or the timeout, so none did, unless a stall lengthened it.
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

/// An open loop against a server that stops answering two of four connections after their fifth response, and every reconnection. A starved slot's requests keep falling due every 20 ms, so after each timeout it has a request due, and its span runs from its fifth response through every timeout and reconnection to its last request's failure, at the drain deadline, a second after the window. A request can wait longer than the window here: one planned just after the point and never sent waits from its planned time to the drain deadline, nearly the window and the drain, so this test does not show, as the closed loop does, that the span finds starvation where no request waited as long as the window.
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

/// How long after the window [`Server::relaying`] holds the last request before the drain deadline ends it: longer than the limit, the whole window, so that the drain alone makes its span longer than the limit.
const RELAY_DRAIN: &str = "3s";
const RELAY_DRAIN_NS: u64 = 3_000_000_000;

/// A closed loop over four connections against a server that answers each request when the next arrives. While the client sends, every request is answered by the next arrival, so every connection is answered in the window. Once the window ends, the client sends nothing more, so exactly one request, the last to arrive, is never answered: it waits until the drain deadline, and its connection's span, from its last response in the window, is longer than the limit only with the drain. The limit is the whole window, and a span's part in the window is never longer than the window, so no connection is starved in the window, however the client or the server stalls, unless a stall covers all of it and leaves a connection unanswered. A stall across the window's end can lengthen other spans open at its end past the limit with the drain, and can hold answers unread until the drain deadline, so the run starves at least the held request's connection, and every starved connection only in the drain. The run is reported as starving them, and why, but `--fail-on starved` keeps it.
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
            CLOSED_WINDOW,
            "--starved-limit",
            "1",
            "--drain-timeout",
            RELAY_DRAIN,
            "--connection-details",
        ],
    )
    .unwrap();
    let starvation = report.starvation();
    let summary = report.summary();
    assert_eq!(
        (starvation.connections, starvation.unanswered),
        (4, 0),
        "{summary}"
    );
    assert!(starvation.starved >= 1, "{summary}");
    assert_eq!(
        (starvation.long_span, starvation.drain_only),
        (starvation.starved, starvation.starved),
        "{summary}"
    );
    // The held request's span runs from its connection's last response, in the window, to the drain deadline.
    assert!(starvation.span_max > RELAY_DRAIN_NS, "{summary}");
    // The held request is never answered, and the drain deadline timed it out.
    let drain = report.statistics().period(Period::Drain);
    assert!(drain.errors[ErrorKind::Timeout as usize] >= 1, "{summary}");
    let reason = report.starved().unwrap();
    assert!(
        reason.contains(&format!(
            "; {} of them only in the drain, after the window: in the window, the server answered them, and their starved spans were within the limit; ",
            starvation.drain_only
        )),
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
    assert_eq!(starved.len(), starvation.starved);
    for connection in &starved {
        assert_eq!(connection["drain_only"], true, "{connection}");
        assert!(connection["valid"].as_u64().unwrap() > 0, "{connection}");
    }
    // The held request's connection waited longest for it, unanswered.
    assert!(
        starved
            .iter()
            .any(|connection| connection["wait_max_unanswered"] == true
                && connection["span_max_ns"].as_u64().unwrap() > RELAY_DRAIN_NS),
        "{starved:?}"
    );
}
