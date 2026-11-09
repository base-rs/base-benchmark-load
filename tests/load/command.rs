// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The `base-benchmark-load` binary: one record on stdout, the summary on stderr, and the exit status.

use super::{
    count,
    server::{Action, HELLO, Server},
};
use base_benchmark::ProcessRecord;
use std::process::{Command, Output};

fn command(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_base-benchmark-load"))
        .args(arguments)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn help_and_version() {
    let output = command(&["help"]);
    assert!(output.status.success());
    assert!(text(&output.stdout).starts_with("Usage: base-benchmark-load MODE"));
    let output = command(&["--version"]);
    assert_eq!(
        text(&output.stdout),
        format!("base-benchmark-load {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn invalid_options_fail_without_a_record() {
    let output = command(&["closed", "--connections", "none"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        text(&output.stderr)
            .starts_with("base-benchmark-load: --connections needs a positive integer")
    );
}

#[test]
fn runs_write_one_record_and_a_summary() {
    let server = Server::hello();
    let address = server.address.to_string();
    let output = command(&[
        "rate",
        "--address",
        &address,
        "--rate",
        "1000",
        "--connections",
        "2",
        "--warmup",
        "0",
        "--duration",
        "50ms",
    ]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert_eq!(stdout.lines().count(), 1);
    let record: ProcessRecord = serde_json::from_str(&stdout).unwrap();
    record.validate().unwrap();
    assert_eq!(count(&record, "responses_valid"), 50);
    assert!(text(&output.stderr).contains("latency: p50"));
    // Quietly, with nothing to fail on. Every planned request is answered validly, however slowly.
    let output = command(&[
        "rate",
        "--address",
        &address,
        "--rate",
        "1000",
        "--connections",
        "1",
        "--warmup",
        "0",
        "--duration",
        "20ms",
        "--quiet",
        "--fail-on",
        "invalid,errors",
    ]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(output.stderr.is_empty());
}

#[test]
fn runs_that_cannot_start_fail_without_a_record() {
    let (_socket, address) = super::server::refusing();
    let address = address.to_string();
    let output = command(&["closed", "--address", &address]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(text(&output.stderr).starts_with(&format!(
        "base-benchmark-load: preflight request to {address}"
    )));
}

#[test]
fn fail_on_classes_suppress_the_record() {
    let server = Server::start(|arrival| {
        let mut response = HELLO.to_vec();
        if arrival.connection > 0 {
            *response.last_mut().unwrap() = b'?';
        }
        Action::Respond(response)
    });
    let address = server.address.to_string();
    let output = command(&[
        "rate",
        "--address",
        &address,
        "--rate",
        "1000",
        "--connections",
        "1",
        "--warmup",
        "0",
        "--duration",
        "20ms",
        "--fail-on",
        "invalid",
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("base-benchmark-load: 20 invalid responses"),
        "{stderr}"
    );
    assert!(
        stderr.contains("20 invalid (body)"),
        "the summary is still written: {stderr}"
    );
}

/// A server with one thread serves one of the two connections and starves the other, so `--fail-on starved` exits unsuccessfully without a record. Whether or not the served connection is answered in the short window, at least one is starved.
#[test]
fn fail_on_starved_suppresses_the_record() {
    let server = Server::pool(1);
    let address = server.address.to_string();
    let output = command(&[
        "closed",
        "--address",
        &address,
        "--connections",
        "2",
        "--warmup",
        "0",
        "--duration",
        "20ms",
        "--drain-timeout",
        "10ms",
        "--fail-on",
        "starved",
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(stderr.contains("\nSTARVED: "), "{stderr}");
    let error = stderr.lines().last().unwrap();
    assert!(
        error.starts_with("base-benchmark-load: ") && error.contains(" of 2 connections ("),
        "{stderr}"
    );
    assert!(error.contains(" starved: "), "{stderr}");
}

/// The server never answers, so there is no valid response whether or not a request was sent in the short window.
#[test]
fn closed_loops_without_a_valid_response_fail() {
    let server = Server::start(|arrival| {
        if arrival.connection == 0 {
            Action::Respond(HELLO.to_vec())
        } else {
            Action::Ignore
        }
    });
    let address = server.address.to_string();
    let output = command(&[
        "closed",
        "--address",
        &address,
        "--connections",
        "1",
        "--warmup",
        "0",
        "--duration",
        "20ms",
        "--timeout",
        "10ms",
        "--drain-timeout",
        "10ms",
        "--quiet",
    ]);
    assert!(!output.status.success());
    assert!(text(&output.stderr).contains("no valid response completed in the measured window"));
}

#[test]
fn rate_loops_without_a_valid_response_fail() {
    // Operations are successes only, so a cohort that failed entirely has none, and no record.
    let server = Server::start(|arrival| {
        if arrival.connection == 0 {
            Action::Respond(HELLO.to_vec())
        } else {
            Action::Close
        }
    });
    let address = server.address.to_string();
    let output = command(&[
        "rate",
        "--address",
        &address,
        "--rate",
        "1000",
        "--connections",
        "2",
        "--warmup",
        "0",
        "--duration",
        "20ms",
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("measured cohort: 20 planned; 0 valid, 0 invalid, 20 failed"),
        "the summary is still written: {stderr}"
    );
    assert!(
        stderr.ends_with(
            "base-benchmark-load: no valid response to a request planned in the measured window\n"
        ),
        "{stderr}"
    );
}
