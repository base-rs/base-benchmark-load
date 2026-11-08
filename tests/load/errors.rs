// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Failed requests are counted by kind, next to the successes, and the run continues.

use super::{
    count, extension, load, outcomes, record,
    server::{Action, Arrival, HELLO, Server},
};

/// A server that answers the preflight connection normally and every other request with `action`.
fn after_preflight(action: Action) -> Server {
    Server::start(move |arrival: Arrival| {
        if arrival.connection == 0 {
            Action::Respond(HELLO.to_vec())
        } else {
            action.clone()
        }
    })
}

/// An open loop over two connections at 200 requests per second for 100 ms, reconnecting after failures.
fn failing(server: &Server, extra: &[&str]) -> base_benchmark::ProcessRecord {
    let mut arguments = vec![
        "--rate",
        "200",
        "--connections",
        "2",
        "--warmup",
        "0",
        "--duration",
        "100ms",
        "--drain-timeout",
        "1s",
    ];
    arguments.extend_from_slice(extra);
    let record = record(&load("rate", server.address, &arguments).unwrap());
    // Every planned request has exactly one outcome.
    assert_eq!(outcomes(&record, "measured"), 20);
    record
}

#[test]
fn unanswered_requests_time_out() {
    let server = after_preflight(Action::Ignore);
    let record = failing(&server, &["--timeout", "20ms"]);
    assert_eq!(count(&record, "responses_valid"), 0);
    assert!(count(&record, "errors_timeout") > 0);
    assert_eq!(
        count(&record, "errors"),
        count(&record, "errors_timeout") + count(&record, "errors_unsent")
    );
}

#[test]
fn resets_are_counted() {
    let server = after_preflight(Action::Reset);
    let record = failing(&server, &[]);
    assert_eq!(count(&record, "responses_valid"), 0);
    assert!(count(&record, "errors_reset") > 0);
    assert!(
        extension(&record)["connections"]["disconnects"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn early_closes_are_counted() {
    let server = after_preflight(Action::Close);
    let record = failing(&server, &[]);
    assert!(count(&record, "errors_eof") + count(&record, "errors_reset") > 0);
    assert_eq!(count(&record, "responses_valid"), 0);
}

#[test]
fn unparsable_responses_are_counted() {
    let server = after_preflight(Action::Respond(b"garbage\r\n\r\n".to_vec()));
    let record = failing(&server, &[]);
    assert_eq!(count(&record, "errors_parse"), 20);
}

#[test]
fn a_closing_response_fails_the_requests_pipelined_behind_it() {
    let closing = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nHello, world!".to_vec();
    let server = after_preflight(Action::Respond(closing));
    // At depth 4 and 1,000 requests per second on one connection, requests queue behind each closing response while it reconnects.
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
            "100ms",
            "--reconnect-delay",
            "5ms",
        ],
    )
    .unwrap();
    let record = record(&report);
    assert_eq!(outcomes(&record, "measured"), 100);
    assert!(count(&record, "responses_valid") > 0);
    assert!(count(&record, "errors_closed") > 0);
    assert!(
        extension(&record)["connections"]["connects"]
            .as_u64()
            .unwrap()
            > 1
    );
}

/// The server accepts the preflight and one connection, then stops listening; the connection fails after five responses. The rest of the cohort cannot be sent: reconnections are refused until the drain deadline.
#[test]
fn requests_that_can_never_be_sent_are_unsent() {
    let server = Server::with_accept_limit(2, |arrival| match arrival.request {
        5 if arrival.connection == 1 => Action::Close,
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
            "100ms",
            "--drain-timeout",
            "50ms",
        ],
    )
    .unwrap();
    let record = record(&report);
    assert_eq!(count(&record, "responses_valid"), 5);
    assert_eq!(count(&record, "errors_eof"), 1);
    assert_eq!(count(&record, "errors_unsent"), 94);
    assert!(count(&record, "connect_errors") > 0);
    assert!(report.summary().contains("94 unsent"));
}
