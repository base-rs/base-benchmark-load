// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Framing and validation options against servers that differ from the plain hello server.

use super::{
    count, extension, load, record,
    server::{Action, HELLO, Server},
};
fn rate(server: &Server, extra: &[&str]) -> base_benchmark::ProcessRecord {
    let mut arguments = vec![
        "--rate",
        "1000",
        "--connections",
        "2",
        "--warmup",
        "0",
        "--duration",
        "50ms",
    ];
    arguments.extend_from_slice(extra);
    record(&load("rate", server.address, &arguments).unwrap())
}

#[test]
fn chunked_responses_are_framed() {
    let server = Server::start(|_| {
        Action::Respond(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7\r\nHello, \r\n6\r\nworld!\r\n0\r\n\r\n".to_vec())
    });
    let record = rate(&server, &["--body", "Hello, world!"]);
    assert_eq!(count(&record, "responses_valid"), 50);
    assert_eq!(record.parameters["validation"]["body"]["length"], 13);
}

#[test]
fn close_delimited_responses_complete_when_the_server_closes() {
    let server = Server::start(|_| {
        Action::RespondAndClose(b"HTTP/1.1 200 OK\r\n\r\nHello, world!".to_vec())
    });
    let record = rate(&server, &[]);
    assert_eq!(count(&record, "responses_valid"), 50);
    assert_eq!(count(&record, "errors"), 0);
    // Every request needed its own connection.
    assert!(
        extension(&record)["connections"]["connects"]
            .as_u64()
            .unwrap()
            >= 50
    );
}

#[test]
fn head_requests_have_no_body() {
    let server = Server::start(|_| {
        Action::Respond(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n".to_vec())
    });
    let record = rate(&server, &["--method", "HEAD", "--any-body"]);
    assert_eq!(count(&record, "responses_valid"), 50);
    assert_eq!(
        record.signature.as_deref(),
        Some("http1 status 200 body any")
    );
}

#[test]
fn request_bodies_and_headers_are_sent() {
    let server = Server::hello();
    let record = rate(
        &server,
        &[
            "--method",
            "POST",
            "--path",
            "/echo",
            "--header",
            "X-Test: yes",
            "--request-body",
            "payload",
        ],
    );
    assert_eq!(count(&record, "responses_valid"), 50);
    // The request is identified without the default Host header, which names the address.
    let request = &record.parameters["request"];
    let identity = "POST /echo HTTP/1.1\r\nX-Test: yes\r\nContent-Length: 7\r\n\r\npayload";
    assert_eq!(request["length"], identity.len());
    assert_eq!(request["host"], "default");
    assert_eq!(request["method"], "POST");
}

#[test]
fn required_headers_and_statuses_are_validated() {
    // Of the 50 requests on the one connection, every third is answered validly, the next with another content type, and the next with another status.
    let server = Server::start(|arrival| {
        let hello = String::from_utf8(HELLO.to_vec()).unwrap();
        Action::Respond(match arrival.request % 3 {
            _ if arrival.connection == 0 => HELLO.to_vec(),
            0 => HELLO.to_vec(),
            1 => hello.replace("text/plain", "text/html").into_bytes(),
            _ => hello.replace("200 OK", "503 Unavailable").into_bytes(),
        })
    });
    let record = rate(
        &server,
        &[
            "--connections",
            "1",
            "--expect-header",
            "content-type: text/plain",
        ],
    );
    assert_eq!(count(&record, "responses_valid"), 17);
    assert_eq!(count(&record, "invalid_header"), 17);
    assert_eq!(count(&record, "invalid_status"), 16);
    assert_eq!(
        record.parameters["validation"]["headers"][0][0],
        "content-type"
    );
}

#[test]
fn whole_responses_can_be_required_byte_for_byte() {
    let path = std::env::temp_dir().join(format!("base-benchmark-load-raw-{}", std::process::id()));
    std::fs::write(&path, HELLO).unwrap();
    let file = path.to_str().unwrap().to_owned();
    let server = Server::start(|arrival| {
        Action::Respond(if arrival.connection < 2 {
            HELLO.to_vec()
        } else {
            String::from_utf8(HELLO.to_vec())
                .unwrap()
                .replace("Content-Type", "content-type")
                .into_bytes()
        })
    });
    // One connection answers exactly, the other with a differently cased header.
    let record = rate(&server, &["--response-file", &file]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(count(&record, "responses_valid"), 25);
    assert_eq!(count(&record, "invalid_response"), 25);
    assert!(
        record
            .signature
            .as_deref()
            .unwrap()
            .starts_with("http1 response 78:")
    );
    assert_eq!(record.parameters["validation"]["body"], "response");
    assert!(record.parameters["validation"]["response"].is_string());
    assert_eq!(extension(&record)["connections"]["connects"], 2);
}

#[test]
fn connection_details_list_every_connection() {
    let server = Server::hello();
    let record = rate(&server, &["--connection-details", "--workers", "2"]);
    let details = extension(&record)["connection_details"].as_array().unwrap();
    assert_eq!(details.len(), 2);
    assert_eq!(details[0]["valid"], 25);
    assert_eq!(details[1]["worker"], 1);
    assert!(details[0]["mean_latency_ns"].as_f64().unwrap() > 0.0);
}
