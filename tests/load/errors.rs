// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Failed requests are counted by kind, next to the successes, and the run continues.

use super::{
    count, errors, extension, load, measured, outcomes, record,
    server::{Action, Arrival, HELLO, Server},
};
use base_benchmark_load::{ErrorKind, Outcomes};
use std::sync::atomic::Ordering;

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

/// An open loop over two connections at 200 requests per second for 100 ms, reconnecting after failures, against a server that never answers validly. It ends once every planned request has an outcome. Operations are successes only, so it has no record; its outcomes are the report's.
fn failing(server: &Server, extra: &[&str]) -> Outcomes {
    let mut arguments = vec![
        "--rate",
        "200",
        "--connections",
        "2",
        "--warmup",
        "0",
        "--duration",
        "100ms",
    ];
    arguments.extend_from_slice(extra);
    let report = load("rate", server.address, &arguments).unwrap();
    let error = report.record().unwrap_err();
    assert_eq!(
        error.to_string(),
        "no valid response to a request planned in the measured window"
    );
    let outcomes = measured(&report);
    // Every planned request has exactly one outcome.
    assert_eq!(outcomes.total(), 20);
    assert_eq!(outcomes.valid, 0);
    outcomes
}

#[test]
fn unanswered_requests_time_out() {
    let server = after_preflight(Action::Ignore);
    let outcomes = failing(&server, &["--timeout", "20ms"]);
    // Nothing is ever answered, so every request times out, however late it was sent.
    assert_eq!(errors(&outcomes, ErrorKind::Timeout), 20);
}

#[test]
fn resets_are_counted() {
    let server = after_preflight(Action::Reset);
    let outcomes = failing(&server, &[]);
    assert_eq!(errors(&outcomes, ErrorKind::Reset), 20);
}

#[test]
fn early_closes_are_counted() {
    let server = after_preflight(Action::Close);
    let outcomes = failing(&server, &[]);
    assert_eq!(
        errors(&outcomes, ErrorKind::Eof) + errors(&outcomes, ErrorKind::Reset),
        20
    );
}

#[test]
fn unparsable_responses_are_counted() {
    let server = after_preflight(Action::Respond(b"garbage\r\n\r\n".to_vec()));
    let outcomes = failing(&server, &[]);
    assert_eq!(errors(&outcomes, ErrorKind::Parse), 20);
}

#[test]
fn a_closing_response_fails_the_requests_pipelined_behind_it() {
    let closing = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nHello, world!".to_vec();
    let server = after_preflight(Action::Respond(closing));
    // At depth 4 and 1,000 requests per second on one connection, at least five requests fall due while the connection waits 5 ms to reconnect after each closing response, so it sends four at once, and the three behind the first are closed by it.
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
    assert!(count(&record, "errors_closed") >= 3);
    assert_eq!(
        count(&record, "errors"),
        count(&record, "errors_closed"),
        "every failure is a request behind a closing response"
    );
    assert!(
        extension(&record)["connections"]["connects"]
            .as_u64()
            .unwrap()
            > 1
    );
}

/// The server accepts the preflight and one connection, then stops listening; the connection closes at its sixth request. So at most six requests can ever be sent: the rest of the cohort is unsent at the drain deadline, since reconnections are refused, however slowly the run goes.
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
    let outcomes = measured(&report);
    assert_eq!(outcomes.total(), 100);
    let unsent = errors(&outcomes, ErrorKind::Unsent);
    assert!(unsent >= 94, "{unsent} unsent");
    assert!(outcomes.valid <= 5);
    assert!(errors(&outcomes, ErrorKind::Eof) <= 1);
    // A request sent but unanswered at the drain deadline times out; nothing else can happen to it.
    assert_eq!(
        outcomes.valid
            + errors(&outcomes, ErrorKind::Eof)
            + errors(&outcomes, ErrorKind::Timeout)
            + unsent,
        100
    );
    assert!(server.requests.load(Ordering::Relaxed) <= 1 + 6);
    assert!(report.summary().contains(&format!("{unsent} unsent")));
}
