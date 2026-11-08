// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

fn expectation(body: Option<&[u8]>, response: Option<&[u8]>) -> Expectation {
    Expectation {
        status: 200,
        headers: Vec::new(),
        body: body.map(<[u8]>::to_vec),
        response: response.map(<[u8]>::to_vec),
    }
}

#[test]
fn signatures_identify_valid_responses() {
    assert_eq!(
        signature(&expectation(Some(b"ok"), None), 200),
        format!("http1 status 200 body 2:{}", digest(b"ok"))
    );
    assert_eq!(
        signature(&expectation(None, None), 204),
        "http1 status 204 body any"
    );
    assert_eq!(
        signature(&expectation(Some(b"ok"), Some(b"raw")), 200),
        format!("http1 response 3:{}", digest(b"raw"))
    );
}

#[test]
fn parameters_describe_the_work() {
    let mut options = Options::new(Mode::Rate);
    options.rate = std::num::NonZeroU64::new(5);
    options.expect_headers = vec![("Content-Type".to_owned(), "text/plain".to_owned())];
    let request = RequestTemplate::new("GET", "/", "host", &[], b"");
    let expectation = expectation(None, None);
    let described = parameters(&options, &request, &expectation);
    assert_eq!(described["rate"], 5);
    assert_eq!(described["arrivals"], "uniform");
    assert_eq!(described["assignment"], "per-connection");
    assert_eq!(described["validation"]["body"], "preflight");
    assert_eq!(
        described["validation"]["headers"],
        json!([["content-type", "text/plain"]])
    );
    assert_eq!(described["request"]["host"], "default");
    assert_eq!(
        described["request"]["digest"],
        digest(b"GET / HTTP/1.1\r\n\r\n")
    );
    assert_eq!(described["request"]["length"], 18);
    // The address varies between equal runs, such as runs against servers on different ports, so the default Host header that names it is not a parameter.
    let elsewhere = RequestTemplate::new("GET", "/", "127.0.0.1:9999", &[], b"");
    assert_eq!(parameters(&options, &elsewhere, &expectation), described);
    // A Host header given explicitly changes the request, so it is.
    let headers = [("Host".to_owned(), "example".to_owned())];
    let explicit = RequestTemplate::new("GET", "/", "host", &headers, b"");
    let explicit = parameters(&options, &explicit, &expectation);
    assert_eq!(explicit["request"]["host"], "header");
    assert_eq!(
        explicit["request"]["digest"],
        digest(b"GET / HTTP/1.1\r\nHost: example\r\n\r\n")
    );
    options.body = BodyExpectation::Any;
    assert_eq!(
        parameters(&options, &request, &expectation)["validation"]["body"],
        "any"
    );
    options.body = BodyExpectation::Exact(b"ok".to_vec());
    assert_eq!(
        parameters(&options, &request, &expectation)["validation"]["body"],
        json!({"length": 2, "digest": digest(b"ok")})
    );
    options.response = Some(b"raw".to_vec());
    assert_eq!(
        parameters(&options, &request, &expectation)["validation"]["body"],
        "response"
    );
}

/// A reader that is interrupted by a signal before every chunk it returns.
struct Interrupted<'a> {
    chunks: std::collections::VecDeque<&'a [u8]>,
    interrupt: bool,
}

impl Read for Interrupted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.interrupt = !self.interrupt;
        if self.interrupt {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let chunk = self.chunks.pop_front().unwrap_or_default();
        buffer[..chunk.len()].copy_from_slice(chunk);
        Ok(chunk.len())
    }
}

#[test]
fn preflight_reads_are_retried_when_interrupted() {
    let request = RequestTemplate::new("GET", "/", "host", &[], b"");
    let reader = Interrupted {
        chunks: [&b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no"[..], b"k"].into(),
        interrupt: false,
    };
    let response = read_response(reader, &request, &expectation(None, None)).unwrap();
    assert_eq!(response.body.as_deref(), Some(&b"ok"[..]));
}

#[test]
fn sleeping_until_a_past_instant_returns_at_once() {
    let clock = MonotonicClock::new();
    sleep_until(&clock, 0);
    let target = clock.now() + 1_000_000;
    sleep_until(&clock, target);
    assert!(clock.now() >= target);
}

#[test]
fn addresses_resolve() {
    assert_eq!(
        resolve("127.0.0.1:80").unwrap(),
        "127.0.0.1:80".parse::<SocketAddr>().unwrap()
    );
    assert!(resolve("localhost:80").unwrap().ip().is_loopback());
}
