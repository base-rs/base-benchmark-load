// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

/// A request serialized once, before the run, and copied for every send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestTemplate {
    pub bytes: Vec<u8>,
    /// Whether the method is `HEAD`, whose responses have no body.
    pub head: bool,
}

impl RequestTemplate {
    /// An HTTP/1.1 request. A `Host` header naming `host` is added unless `headers` has one, and `Content-Length` when there is a body. Inputs are validated by the options; they contain no CR or LF.
    pub fn new(
        method: &str,
        target: &str,
        host: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Self {
        let mut bytes = format!("{method} {target} HTTP/1.1\r\n").into_bytes();
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("host"))
        {
            bytes.extend_from_slice(format!("Host: {host}\r\n").as_bytes());
        }
        for (name, value) in headers {
            bytes.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        if !body.is_empty() {
            bytes.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        bytes.extend_from_slice(body);
        Self {
            bytes,
            head: method == "HEAD",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_the_default_request_as_the_fixed_rate_client_did() {
        let request = RequestTemplate::new("GET", "/", "127.0.0.1:8080", &[], b"");
        assert_eq!(
            request.bytes,
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n\r\n"
        );
        assert!(!request.head);
    }

    #[test]
    fn adds_headers_and_a_body_length() {
        let headers = [
            ("host".to_owned(), "example".to_owned()),
            ("Accept".to_owned(), "*/*".to_owned()),
        ];
        let request = RequestTemplate::new("POST", "/echo", "ignored", &headers, b"hi");
        assert_eq!(
            request.bytes,
            b"POST /echo HTTP/1.1\r\nhost: example\r\nAccept: */*\r\nContent-Length: 2\r\n\r\nhi"
        );
        assert!(RequestTemplate::new("HEAD", "/", "h", &[], b"").head);
    }
}
