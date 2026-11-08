// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! HTTP/1.1 response framing and validation.
//!
//! Heads are parsed by `httparse`; bodies are framed here, following RFC 9112 section 6.3: no body for `HEAD` requests and `204` or `304` responses, chunked transfer coding when it is the final coding, any other transfer coding or no length delimited by the connection closing, and otherwise `Content-Length`. This framing is deliberately independent of Base's own HTTP implementation, so that a framing bug shared by client and server cannot hide itself.

use crate::statistics::InvalidKind;

/// The largest response head accepted. A longer head is a parse error, which bounds the receive buffer.
pub const HEAD_SIZE_LIMIT: usize = 64 * 1024;
const HEADER_COUNT_LIMIT: usize = 100;
/// The longest chunk-size or trailer line accepted.
const LINE_LENGTH_LIMIT: usize = 4096;

/// What a valid response must contain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expectation {
    pub status: u16,
    /// Required headers: lowercase names and exact values, compared after trimming surrounding whitespace. Names match case-insensitively.
    pub headers: Vec<(String, Vec<u8>)>,
    /// The exact body, or `None` to accept any correctly framed body.
    pub body: Option<Vec<u8>>,
    /// The exact bytes of the whole response, head included, for raw controls.
    pub response: Option<Vec<u8>>,
}

/// Why a response could not be framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Head,
    HeadSizeLimit,
    ContentLength,
    ChunkSize,
    ChunkTerminator,
    LineLengthLimit,
    /// A `101 Switching Protocols` response, which ends HTTP/1.1 on the connection.
    Switching,
}

/// A complete response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// Why the response is invalid, or `None` when it is valid.
    pub invalid: Option<InvalidKind>,
    /// Whether the server will close the connection after this response.
    pub close: bool,
    pub body_length: u64,
    /// The body, when the parser was created with [`ResponseParser::capturing`].
    pub body: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Head,
    Length(u64),
    ChunkSize,
    ChunkData(u64),
    ChunkEnd,
    Trailer,
    Close,
}

/// Compares bytes as they arrive with an optional expected sequence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Matcher {
    offset: usize,
    differs: bool,
}

impl Matcher {
    fn feed(&mut self, expected: Option<&[u8]>, bytes: &[u8]) {
        if let Some(expected) = expected {
            self.differs |= expected.get(self.offset..self.offset + bytes.len()) != Some(bytes);
        }
        self.offset += bytes.len();
    }

    fn matches(&self, expected: Option<&[u8]>) -> bool {
        expected.is_none_or(|expected| !self.differs && self.offset == expected.len())
    }
}

/// An incremental response parser for one connection. It consumes bytes as they arrive, keeping only its framing state, so bodies are validated without being buffered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseParser {
    stage: Stage,
    status: u16,
    headers_match: bool,
    close: bool,
    body: Matcher,
    response: Matcher,
    capture: Option<Vec<u8>>,
}

impl Default for ResponseParser {
    fn default() -> Self {
        Self {
            stage: Stage::Head,
            status: 0,
            headers_match: true,
            close: false,
            body: Matcher::default(),
            response: Matcher::default(),
            capture: None,
        }
    }
}

impl ResponseParser {
    /// A parser that also returns each response's body, for the preflight request that may provide the expected body.
    pub fn capturing() -> Self {
        Self {
            capture: Some(Vec::new()),
            ..Self::default()
        }
    }

    /// Parse the unconsumed bytes received on the connection, `input`, as the response to a request, which was a `HEAD` request when `head_request` is set.
    ///
    /// Returns how many bytes were consumed, and the response if they completed one. Call again with the remaining bytes after a complete response, which may be the next pipelined response. An incomplete response consumes what it can and returns `None`; the bytes of an incomplete head are left unconsumed until the head is complete.
    pub fn parse(
        &mut self,
        input: &[u8],
        expectation: &Expectation,
        head_request: bool,
    ) -> Result<(usize, Option<Response>), ParseError> {
        let mut consumed = 0;
        loop {
            let rest = &input[consumed..];
            match self.stage {
                Stage::Head => {
                    let mut headers = [httparse::EMPTY_HEADER; HEADER_COUNT_LIMIT];
                    let mut head = httparse::Response::new(&mut headers);
                    let length = match head.parse(rest) {
                        Ok(httparse::Status::Complete(length)) => length,
                        Ok(httparse::Status::Partial) if rest.len() < HEAD_SIZE_LIMIT => {
                            return Ok((consumed, None));
                        }
                        Ok(httparse::Status::Partial) => return Err(ParseError::HeadSizeLimit),
                        Err(_) => return Err(ParseError::Head),
                    };
                    let status = head
                        .code
                        .expect("a complete response head has a status code");
                    self.response
                        .feed(expectation.response.as_deref(), &rest[..length]);
                    consumed += length;
                    if status == 101 {
                        return Err(ParseError::Switching);
                    }
                    if status < 200 {
                        // An interim response; the final response follows it.
                        continue;
                    }
                    self.status = status;
                    self.headers_match = expectation.headers.iter().all(|(name, value)| {
                        head.headers.iter().any(|header| {
                            header.name.eq_ignore_ascii_case(name)
                                && header.value.trim_ascii() == value.as_slice()
                        })
                    });
                    self.close = closes(&head);
                    self.stage = framing(&head, status, head_request)?;
                }
                Stage::Length(0) => return Ok((consumed, Some(self.finish(expectation)))),
                Stage::Length(remaining) => {
                    let taken = self.take_body(expectation, rest, remaining);
                    if taken == 0 {
                        return Ok((consumed, None));
                    }
                    consumed += taken;
                    self.stage = Stage::Length(remaining - taken as u64);
                }
                Stage::ChunkSize => {
                    let Some(line) = line(rest)? else {
                        return Ok((consumed, None));
                    };
                    let size = chunk_size(&rest[..line]).ok_or(ParseError::ChunkSize)?;
                    self.response
                        .feed(expectation.response.as_deref(), &rest[..line + 2]);
                    consumed += line + 2;
                    self.stage = if size == 0 {
                        Stage::Trailer
                    } else {
                        Stage::ChunkData(size)
                    };
                }
                Stage::ChunkData(0) => self.stage = Stage::ChunkEnd,
                Stage::ChunkData(remaining) => {
                    let taken = self.take_body(expectation, rest, remaining);
                    if taken == 0 {
                        return Ok((consumed, None));
                    }
                    consumed += taken;
                    self.stage = Stage::ChunkData(remaining - taken as u64);
                }
                Stage::ChunkEnd => {
                    if rest.len() < 2 {
                        return Ok((consumed, None));
                    }
                    if &rest[..2] != b"\r\n" {
                        return Err(ParseError::ChunkTerminator);
                    }
                    self.response
                        .feed(expectation.response.as_deref(), &rest[..2]);
                    consumed += 2;
                    self.stage = Stage::ChunkSize;
                }
                Stage::Trailer => {
                    let Some(line) = line(rest)? else {
                        return Ok((consumed, None));
                    };
                    self.response
                        .feed(expectation.response.as_deref(), &rest[..line + 2]);
                    consumed += line + 2;
                    if line == 0 {
                        return Ok((consumed, Some(self.finish(expectation))));
                    }
                }
                Stage::Close => {
                    self.take_body(expectation, rest, u64::MAX);
                    return Ok((input.len(), None));
                }
            }
        }
    }

    /// The connection was closed: complete a response delimited by the close, if one is in progress.
    pub fn at_eof(&mut self, expectation: &Expectation) -> Option<Response> {
        (self.stage == Stage::Close).then(|| self.finish(expectation))
    }

    fn take_body(&mut self, expectation: &Expectation, rest: &[u8], remaining: u64) -> usize {
        let taken = rest
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let bytes = &rest[..taken];
        self.body.feed(expectation.body.as_deref(), bytes);
        self.response.feed(expectation.response.as_deref(), bytes);
        if let Some(capture) = &mut self.capture {
            capture.extend_from_slice(bytes);
        }
        taken
    }

    fn finish(&mut self, expectation: &Expectation) -> Response {
        let invalid = if self.status != expectation.status {
            Some(InvalidKind::Status)
        } else if !self.headers_match {
            Some(InvalidKind::Header)
        } else if !self.body.matches(expectation.body.as_deref()) {
            Some(InvalidKind::Body)
        } else if !self.response.matches(expectation.response.as_deref()) {
            Some(InvalidKind::Response)
        } else {
            None
        };
        let response = Response {
            status: self.status,
            invalid,
            close: self.close,
            body_length: self.body.offset as u64,
            body: self.capture.take(),
        };
        // Start the next response, still capturing if this one did.
        *self = Self {
            capture: response.body.as_ref().map(|_| Vec::new()),
            ..Self::default()
        };
        response
    }
}

/// The length of the first line of `rest`, before its CRLF, if it is complete.
fn line(rest: &[u8]) -> Result<Option<usize>, ParseError> {
    match rest.windows(2).position(|pair| pair == b"\r\n") {
        Some(length) if length <= LINE_LENGTH_LIMIT => Ok(Some(length)),
        None if rest.len() <= LINE_LENGTH_LIMIT => Ok(None),
        _ => Err(ParseError::LineLengthLimit),
    }
}

/// A chunk-size line: hexadecimal digits, then optional whitespace and chunk extensions, which are ignored.
fn chunk_size(line: &[u8]) -> Option<u64> {
    let digits = line
        .iter()
        .position(|byte| !byte.is_ascii_hexdigit())
        .unwrap_or(line.len());
    let rest = &line[digits..];
    let rest = rest.trim_ascii_start();
    if digits == 0 || digits > 16 || !(rest.is_empty() || rest[0] == b';') {
        return None;
    }
    let digits = std::str::from_utf8(&line[..digits]).expect("hexadecimal digits are ASCII");
    Some(u64::from_str_radix(digits, 16).expect("at most 16 hexadecimal digits fit"))
}

/// A decimal length of at most 19 digits, so that it fits.
fn decimal(value: &[u8]) -> Option<u64> {
    let valid = !value.is_empty() && value.len() <= 19 && value.iter().all(u8::is_ascii_digit);
    valid.then(|| {
        std::str::from_utf8(value)
            .expect("decimal digits are ASCII")
            .parse()
            .expect("at most 19 decimal digits fit")
    })
}

fn tokens(value: &[u8]) -> impl Iterator<Item = &[u8]> {
    value
        .split(|byte| *byte == b',')
        .map(<[u8]>::trim_ascii)
        .filter(|token| !token.is_empty())
}

fn framing(
    head: &httparse::Response,
    status: u16,
    head_request: bool,
) -> Result<Stage, ParseError> {
    if head_request || status == 204 || status == 304 {
        return Ok(Stage::Length(0));
    }
    let mut coding = None;
    let mut length = None;
    for header in head.headers.iter() {
        if header.name.eq_ignore_ascii_case("transfer-encoding") {
            coding = tokens(header.value).last().or(coding);
        } else if header.name.eq_ignore_ascii_case("content-length") {
            for token in tokens(header.value) {
                let value = decimal(token).ok_or(ParseError::ContentLength)?;
                if length.is_some_and(|length| length != value) {
                    return Err(ParseError::ContentLength);
                }
                length = Some(value);
            }
        }
    }
    Ok(match (coding, length) {
        (Some(coding), _) if coding.eq_ignore_ascii_case(b"chunked") => Stage::ChunkSize,
        (Some(_), _) | (None, None) => Stage::Close,
        (None, Some(length)) => Stage::Length(length),
    })
}

/// Whether the server will close the connection after this response: `Connection: close`, or HTTP/1.0 without `Connection: keep-alive`.
fn closes(head: &httparse::Response) -> bool {
    let has = |option: &[u8]| {
        head.headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("connection"))
            .flat_map(|header| tokens(header.value))
            .any(|token| token.eq_ignore_ascii_case(option))
    };
    has(b"close") || (head.version == Some(0) && !has(b"keep-alive"))
}

#[cfg(test)]
mod tests;
