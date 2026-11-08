// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

const HELLO: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nHello, world!";

fn expect(body: Option<&[u8]>) -> Expectation {
    Expectation {
        status: 200,
        body: body.map(<[u8]>::to_vec),
        ..Expectation::default()
    }
}

/// Parse `input` completely, as one buffer, returning every response and the bytes left unconsumed.
fn parse_all(
    input: &[u8],
    expectation: &Expectation,
    head_request: bool,
) -> Result<(Vec<Response>, usize), ParseError> {
    let mut parser = ResponseParser::default();
    let mut responses = Vec::new();
    let mut offset = 0;
    loop {
        let (consumed, response) = parser.parse(&input[offset..], expectation, head_request)?;
        offset += consumed;
        match response {
            Some(response) => responses.push(response),
            None => return Ok((responses, input.len() - offset)),
        }
    }
}

/// Feed `input` one byte at a time, as a connection would if every read returned one byte, keeping unconsumed bytes as a connection's buffer does.
fn parse_bytewise(input: &[u8], expectation: &Expectation) -> Vec<Response> {
    let mut parser = ResponseParser::default();
    let mut buffer = Vec::new();
    let mut responses = Vec::new();
    for byte in input {
        buffer.push(*byte);
        loop {
            let (consumed, response) = parser.parse(&buffer, expectation, false).unwrap();
            buffer.drain(..consumed);
            match response {
                Some(response) => responses.push(response),
                None => break,
            }
        }
    }
    assert!(buffer.is_empty());
    responses
}

fn valid(body_length: u64) -> Response {
    Response {
        status: 200,
        invalid: None,
        close: false,
        body_length,
        body: None,
    }
}

#[test]
fn frames_a_content_length_response() {
    let (responses, left) = parse_all(HELLO, &expect(Some(b"Hello, world!")), false).unwrap();
    assert_eq!(responses, [valid(13)]);
    assert_eq!(left, 0);
}

#[test]
fn frames_responses_split_at_every_byte() {
    let mut input = HELLO.to_vec();
    input.extend_from_slice(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nHello\r\n8;name=value\r\n, world!\r\n0\r\nTrailer: yes\r\n\r\n");
    input.extend_from_slice(HELLO);
    let responses = parse_bytewise(&input, &expect(Some(b"Hello, world!")));
    assert_eq!(responses, [valid(13), valid(13), valid(13)]);
}

#[test]
fn frames_pipelined_responses_in_one_buffer() {
    let mut input = HELLO.repeat(3);
    input.extend_from_slice(&HELLO[..20]);
    let (responses, left) = parse_all(&input, &expect(Some(b"Hello, world!")), false).unwrap();
    assert_eq!(responses.len(), 3);
    // The incomplete fourth head stays unconsumed.
    assert_eq!(left, 20);
}

#[test]
fn bodiless_responses() {
    let expectation = expect(Some(b""));
    for input in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n"[..],
        b"HTTP/1.1 200 OK\r\n\r\n",
    ] {
        // A response to `HEAD` has no body, whatever its headers say.
        assert_eq!(parse_all(input, &expectation, true).unwrap().0, [valid(0)]);
    }
    for status in [204, 304] {
        let input = format!("HTTP/1.1 {status} X\r\nContent-Length: 13\r\n\r\n");
        let expectation = Expectation {
            status,
            ..expectation.clone()
        };
        let (responses, left) = parse_all(input.as_bytes(), &expectation, false).unwrap();
        assert_eq!(responses[0].status, status);
        assert_eq!(responses[0].invalid, None);
        assert_eq!(left, 0);
    }
}

#[test]
fn interim_responses_precede_the_final_response() {
    let input = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </a>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    let mut parser = ResponseParser::default();
    let expectation = expect(Some(b"ok"));
    let (consumed, response) = parser.parse(&input[..30], &expectation, false).unwrap();
    assert_eq!((consumed, response), (25, None));
    let (consumed, response) = parser.parse(&input[25..], &expectation, false).unwrap();
    assert_eq!(consumed, input.len() - 25);
    assert_eq!(response, Some(valid(2)));
}

#[test]
fn close_delimited_bodies_complete_at_eof() {
    let expectation = expect(Some(b"until close"));
    for input in [
        &b"HTTP/1.1 200 OK\r\n\r\nuntil close"[..],
        // A final coding other than chunked is delimited by the close too.
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nuntil close",
    ] {
        let mut parser = ResponseParser::default();
        assert_eq!(parser.at_eof(&expectation), None);
        let (consumed, response) = parser.parse(input, &expectation, false).unwrap();
        assert_eq!((consumed, response), (input.len(), None));
        let (consumed, response) = parser.parse(b"", &expectation, false).unwrap();
        assert_eq!((consumed, response), (0, None));
        assert_eq!(parser.at_eof(&expectation), Some(valid(11)));
    }
}

#[test]
fn transfer_encoding_takes_the_last_coding_of_every_header() {
    let input = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nTransfer-Encoding: , CHUNKED \r\n\r\n2\r\nok\r\n0\r\n\r\n";
    let (responses, _) = parse_all(input, &expect(Some(b"ok")), false).unwrap();
    assert_eq!(responses, [valid(2)]);
    let input =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding:\r\n\r\n0\r\n\r\n";
    assert_eq!(
        parse_all(input, &expect(Some(b"")), false).unwrap().0,
        [valid(0)]
    );
}

#[test]
fn content_lengths_must_agree_and_be_decimal() {
    let input = b"HTTP/1.1 200 OK\r\nContent-Length: 2, 2\r\nContent-Length: 2\r\n\r\nok";
    assert_eq!(
        parse_all(input, &expect(Some(b"ok")), false).unwrap().0,
        [valid(2)]
    );
    // An empty value names no length, so it is an error rather than leaving the body to be delimited by the close.
    for header in ["2, 3", "+2", "0x2", "", " , ", "99999999999999999999"] {
        let input =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {header}\r\nContent-Length: 2\r\n\r\nok");
        let result = parse_all(input.as_bytes(), &expect(None), false);
        assert_eq!(result.unwrap_err(), ParseError::ContentLength, "{header:?}");
    }
    let alone = b"HTTP/1.1 200 OK\r\nContent-Length:\r\n\r\nok";
    assert_eq!(
        parse_all(alone, &expect(None), false).unwrap_err(),
        ParseError::ContentLength
    );
}

/// RFC 9112 section 6.3: a message with both `Transfer-Encoding` and `Content-Length` ought to be handled as an error, whatever the codings, since the two framings may disagree.
#[test]
fn transfer_encoding_with_content_length_is_a_parse_error() {
    for coding in ["chunked", "gzip", ""] {
        for (first, second) in [
            (format!("Transfer-Encoding: {coding}"), "Content-Length: 2"),
            (
                "Content-Length: 2".to_owned(),
                &*format!("Transfer-Encoding: {coding}"),
            ),
        ] {
            let input =
                format!("HTTP/1.1 200 OK\r\n{first}\r\n{second}\r\n\r\n2\r\nok\r\n0\r\n\r\n");
            assert_eq!(
                parse_all(input.as_bytes(), &expect(None), false).unwrap_err(),
                ParseError::TransferEncodingWithContentLength,
                "{input:?}"
            );
        }
    }
    // Responses without a body are not framed by either field.
    let input =
        b"HTTP/1.1 204 No Content\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n";
    assert!(parse_all(input, &expect(None), false).is_ok());
}

#[test]
fn malformed_heads_are_parse_errors() {
    assert_eq!(
        parse_all(b"HTTP/1.1 OK\r\n\r\n", &expect(None), false).unwrap_err(),
        ParseError::Head
    );
    assert_eq!(
        parse_all(
            b"HTTP/1.1 101 Switching Protocols\r\n\r\n",
            &expect(None),
            false
        )
        .unwrap_err(),
        ParseError::Switching
    );
    let mut long = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
    long.resize(HEAD_SIZE_LIMIT - 1, b'a');
    assert_eq!(
        parse_all(&long, &expect(None), false).unwrap(),
        (vec![], long.len())
    );
    long.push(b'a');
    assert_eq!(
        parse_all(&long, &expect(None), false).unwrap_err(),
        ParseError::HeadSizeLimit
    );
}

#[test]
fn malformed_chunks_are_parse_errors() {
    let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    for (chunks, error) in [
        ("x\r\n", ParseError::ChunkSize),
        (";\r\n", ParseError::ChunkSize),
        ("1 x\r\n", ParseError::ChunkSize),
        ("00000000000000001\r\n", ParseError::ChunkSize),
        ("1\r\nxyz", ParseError::ChunkTerminator),
    ] {
        let input = format!("{head}{chunks}");
        assert_eq!(
            parse_all(input.as_bytes(), &expect(None), false).unwrap_err(),
            error,
            "{chunks:?}"
        );
    }
    // Whitespace before an extension is allowed, and sixteen digits fit.
    let input = format!("{head}1 \t;x\r\nz\r\n0000000000000000\r\n\r\n");
    assert_eq!(
        parse_all(input.as_bytes(), &expect(Some(b"z")), false)
            .unwrap()
            .0,
        [valid(1)]
    );
}

#[test]
fn long_lines_are_parse_errors() {
    let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    let long = "1".repeat(LINE_LENGTH_LIMIT + 1);
    // A line too long to be complete yet.
    let input = format!("{head}{long}");
    assert_eq!(
        parse_all(input.as_bytes(), &expect(None), false).unwrap_err(),
        ParseError::LineLengthLimit
    );
    // A complete line that is too long.
    let input = format!("{head}0\r\nX: {long}\r\n\r\n");
    assert_eq!(
        parse_all(input.as_bytes(), &expect(None), false).unwrap_err(),
        ParseError::LineLengthLimit
    );
    // A line at the limit is incomplete until its CRLF arrives.
    let input = format!("{head}0\r\n{}", "x".repeat(LINE_LENGTH_LIMIT));
    assert_eq!(
        parse_all(input.as_bytes(), &expect(None), false).unwrap().1,
        LINE_LENGTH_LIMIT
    );
}

#[test]
fn incomplete_chunk_terminators_wait_for_more_input() {
    let input = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nz\r";
    assert_eq!(parse_all(input, &expect(None), false).unwrap(), (vec![], 1));
}

#[test]
fn validation_reports_the_first_failed_check() {
    let mut expectation = Expectation {
        status: 200,
        headers: vec![("content-type".to_owned(), b"text/plain".to_vec())],
        body: Some(b"Hello, world!".to_vec()),
        response: Some(HELLO.to_vec()),
    };
    let invalid = |input: &[u8], expectation: &Expectation| {
        let (_, response) = ResponseParser::default()
            .parse(input, expectation, false)
            .unwrap();
        response.unwrap().invalid
    };
    assert_eq!(invalid(HELLO, &expectation), None);
    let other = String::from_utf8(HELLO.to_vec()).unwrap();
    assert_eq!(
        invalid(other.replace("200", "500").as_bytes(), &expectation),
        Some(InvalidKind::Status)
    );
    assert_eq!(
        invalid(
            other.replace("text/plain", "text/html!").as_bytes(),
            &expectation
        ),
        Some(InvalidKind::Header)
    );
    assert_eq!(
        invalid(
            other
                .replace("Content-Type: text/plain", "Server: x")
                .as_bytes(),
            &expectation
        ),
        Some(InvalidKind::Header)
    );
    assert_eq!(
        invalid(other.replace("world", "World").as_bytes(), &expectation),
        Some(InvalidKind::Body)
    );
    // A shorter or longer body differs too.
    assert_eq!(
        invalid(other.replace("13", "12").as_bytes(), &expectation),
        Some(InvalidKind::Body)
    );
    let longer = other.replace("13", "14") + "!";
    assert_eq!(
        invalid(longer.as_bytes(), &expectation),
        Some(InvalidKind::Body)
    );
    // Header names match case-insensitively and values after trimming, but raw bytes must be exact.
    let recased = other.replace("Content-Type: text/plain", "content-type:  text/plain ");
    assert_eq!(
        invalid(recased.as_bytes(), &expectation),
        Some(InvalidKind::Response)
    );
    expectation.response = None;
    assert_eq!(invalid(recased.as_bytes(), &expectation), None);
    expectation.body = None;
    assert_eq!(
        invalid(other.replace("world", "World").as_bytes(), &expectation),
        None
    );
}

#[test]
fn raw_validation_covers_chunk_framing_and_trailers() {
    let input =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\nX: 1\r\n\r\n";
    let expectation = Expectation {
        status: 200,
        response: Some(input.to_vec()),
        ..Expectation::default()
    };
    assert_eq!(
        parse_all(input, &expectation, false).unwrap().0[0].invalid,
        None
    );
    let changed = String::from_utf8(input.to_vec())
        .unwrap()
        .replace("X: 1", "X: 2");
    assert_eq!(
        parse_all(changed.as_bytes(), &expectation, false)
            .unwrap()
            .0[0]
            .invalid,
        Some(InvalidKind::Response)
    );
}

#[test]
fn closing_responses() {
    let close = |head: &str| parse_all(head.as_bytes(), &expect(None), false).unwrap().0[0].close;
    assert!(close(
        "HTTP/1.1 200 OK\r\nConnection: keep-alive, Close\r\nContent-Length: 0\r\n\r\n"
    ));
    assert!(close("HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n"));
    assert!(!close(
        "HTTP/1.0 200 OK\r\nConnection: Keep-Alive\r\nContent-Length: 0\r\n\r\n"
    ));
    assert!(!close(
        "HTTP/1.1 200 OK\r\nConnection: upgrade\r\nContent-Length: 0\r\n\r\n"
    ));
}

#[test]
fn capturing_parsers_return_each_body() {
    let mut parser = ResponseParser::capturing();
    let input = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokHTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nyes";
    let expectation = expect(None);
    let (consumed, first) = parser.parse(input, &expectation, false).unwrap();
    assert_eq!(first.unwrap().body, Some(b"ok".to_vec()));
    let (_, second) = parser
        .parse(&input[consumed..], &expectation, false)
        .unwrap();
    assert_eq!(second.unwrap().body, Some(b"yes".to_vec()));
}

#[test]
fn chunk_sizes_and_decimals() {
    assert_eq!(chunk_size(b"fF"), Some(255));
    assert_eq!(chunk_size(b"ffffffffffffffff"), Some(u64::MAX));
    assert_eq!(
        decimal(b"9999999999999999999"),
        Some(9_999_999_999_999_999_999)
    );
    assert_eq!(decimal(b""), None);
}
