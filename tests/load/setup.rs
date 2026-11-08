// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Failures before any load is measured: these fail the run instead of producing a record.

use super::{
    load, options,
    server::{Action, HELLO, Server},
};
use std::io;

fn failure(server: &Server, extra: &[&str]) -> io::Error {
    let mut arguments = vec![
        "--rate",
        "100",
        "--connections",
        "1",
        "--warmup",
        "0",
        "--duration",
        "20ms",
    ];
    arguments.extend_from_slice(extra);
    load("rate", server.address, &arguments).unwrap_err()
}

fn contains(error: &io::Error, text: &str) {
    assert!(error.to_string().contains(text), "{error} lacks {text:?}");
}

#[test]
fn unresolvable_addresses() {
    let error =
        base_benchmark_load::run(&options(&["closed", "--address", "no-port"])).unwrap_err();
    contains(&error, "resolving no-port");
}

#[test]
fn refused_preflights() {
    let (_socket, address) = super::server::refusing();
    let error = load("closed", address, &[]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    contains(&error, &format!("preflight request to {address}"));
}

#[test]
fn invalid_preflight_responses() {
    let server = Server::start(|_| {
        Action::Respond(
            String::from_utf8(HELLO.to_vec())
                .unwrap()
                .replace("200 OK", "404 Not Found")
                .into_bytes(),
        )
    });
    contains(
        &failure(&server, &[]),
        "the preflight response is invalid (status): status 404",
    );
    let server = Server::hello();
    contains(&failure(&server, &["--body", "Goodbye"]), "invalid (body)");
}

#[test]
fn incomplete_preflight_responses() {
    let server = Server::start(|_| Action::Close);
    let error = failure(&server, &[]);
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    contains(&error, "before a complete response");
    let server = Server::start(|_| {
        Action::Respond(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nHello".to_vec())
    });
    let error = failure(&server, &["--connect-timeout", "20ms"]);
    contains(&error, "preflight request");
    let server = Server::start(|_| Action::Respond(b"HTTP/1.1 OK\r\n\r\n".to_vec()));
    contains(&failure(&server, &[]), "could not be framed: Head");
}

#[test]
fn preflights_read_responses_in_pieces() {
    let long = String::from_utf8(HELLO.to_vec()).unwrap().replace(
        "Content-Type: text/plain",
        &format!("X-Padding: {}", "x".repeat(40_000)),
    );
    let server = Server::start(move |_| Action::Respond(long.clone().into_bytes()));
    let report = load(
        "rate",
        server.address,
        &[
            "--rate",
            "100",
            "--connections",
            "1",
            "--warmup",
            "0",
            "--duration",
            "20ms",
        ],
    )
    .unwrap();
    assert_eq!(
        report
            .statistics()
            .period(base_benchmark_load::Period::Measured)
            .valid,
        2
    );
}

#[test]
fn workers_that_cannot_pin_fail_the_run() {
    let server = Server::hello();
    contains(
        &failure(&server, &["--cpus", "1023"]),
        "pinning to CPU 1023",
    );
}

#[test]
fn controllers_that_cannot_pin_fail_the_run() {
    let server = Server::hello();
    // Pinning applies to the calling thread, so run it on its own.
    let error = std::thread::spawn(move || failure(&server, &["--controller-cpu", "1023"]))
        .join()
        .unwrap();
    contains(&error, "pinning to CPU 1023");
}

#[test]
fn workers_that_cannot_connect_fail_the_run() {
    // The server accepts only the preflight connection.
    let server = Server::with_accept_limit(1, |_| Action::Respond(HELLO.to_vec()));
    let error = failure(&server, &["--connections", "2", "--workers", "2"]);
    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    contains(&error, &format!("connecting to {}", server.address));
}
