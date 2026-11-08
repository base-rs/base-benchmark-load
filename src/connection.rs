// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{
    clock::Clock,
    response::{Expectation, Response, ResponseParser},
    schedule::{Schedule, count_below},
    statistics::{ConnectionCounters, ErrorKind, Period, Request, Timing, WorkerStatistics},
};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
};

/// Each connection's receive buffer to begin with, as wrk's. It grows only to hold one incomplete head, which the parser bounds.
const INITIAL_INPUT_SIZE: usize = 8 * 1024;

/// What every connection of a run shares: the request, the expectation, and the timing rules.
#[derive(Debug)]
pub struct Settings<'a> {
    pub timing: Timing,
    /// The open-loop schedule, or `None` for the closed loop.
    pub schedule: Option<Schedule>,
    /// The number of planned requests in the open loop.
    pub total: u64,
    /// The first planned request in the measured window.
    pub measured_first: u64,
    /// How many requests may be outstanding on one connection.
    pub depth: usize,
    /// The number of connections, which is the stride between one connection's ordinals.
    pub connections: u64,
    pub timeout: u64,
    pub connect_timeout: u64,
    pub reconnect_delay: u64,
    /// When requests still outstanding are abandoned.
    pub drain_deadline: u64,
    pub request: &'a [u8],
    pub head: bool,
    pub expectation: &'a Expectation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// A connection attempt started at `since`.
    Connecting {
        since: u64,
    },
    Open,
    /// Not connected; the next attempt may start at `until`.
    Down {
        until: u64,
    },
}

/// One persistent connection: its socket, its outstanding requests in order, and the framing state of the response being received.
#[derive(Debug)]
pub struct Connection<T> {
    pub stream: Option<T>,
    pub state: State,
    /// The next ordinal this connection sends in the open loop.
    pub next_ordinal: u64,
    pub in_flight: VecDeque<Request>,
    pub counters: ConnectionCounters,
    output: Vec<u8>,
    written: usize,
    input: Vec<u8>,
    start: usize,
    end: usize,
    parser: ResponseParser,
    /// When the connection last gained room for another outstanding request.
    available: u64,
}

impl<T: Read + Write> Connection<T> {
    /// Connection number `index`, which first sends ordinal `index` in the open loop.
    pub fn new(index: u64) -> Self {
        Self {
            stream: None,
            state: State::Down { until: 0 },
            next_ordinal: index,
            in_flight: VecDeque::new(),
            counters: ConnectionCounters::default(),
            output: Vec::new(),
            written: 0,
            input: vec![0; INITIAL_INPUT_SIZE],
            start: 0,
            end: 0,
            parser: ResponseParser::default(),
            available: 0,
        }
    }

    pub fn connecting(&mut self, stream: T, now: u64) {
        self.stream = Some(stream);
        self.state = State::Connecting { since: now };
    }

    pub fn connected(&mut self, now: u64, statistics: &mut WorkerStatistics) {
        self.state = State::Open;
        self.available = now;
        statistics.connects += 1;
    }

    pub fn connect_failed(
        &mut self,
        now: u64,
        settings: &Settings,
        statistics: &mut WorkerStatistics,
    ) {
        self.stream = None;
        self.state = State::Down {
            until: now.saturating_add(settings.reconnect_delay),
        };
        statistics.connect_errors += 1;
    }

    /// Whether the connection still has requests to send: in the open loop, planned requests it has not sent; in the closed loop, until the window ends.
    pub fn has_work(&self, now: u64, settings: &Settings) -> bool {
        match settings.schedule {
            Some(_) => self.next_ordinal < settings.total,
            None => now < settings.timing.window_end,
        }
    }

    /// Queue every request the connection may send now, up to its depth: in the open loop, its planned requests with ordinals below `due`; in the closed loop, new requests until the window ends. Returns how many were queued; [`Connection::flush`] writes them.
    pub fn dispatch(
        &mut self,
        now: u64,
        due: u64,
        settings: &Settings,
        statistics: &mut WorkerStatistics,
    ) -> usize {
        if self.state != State::Open {
            return 0;
        }
        let mut dispatched = 0;
        while self.in_flight.len() < settings.depth {
            let planned = match settings.schedule {
                Some(schedule) if self.next_ordinal < due => {
                    settings.timing.start + schedule.offset(self.next_ordinal)
                }
                None if now < settings.timing.window_end => settings.timing.start,
                _ => break,
            };
            self.in_flight.push_back(Request {
                ordinal: self.next_ordinal,
                planned,
                sent: now,
                eligible: planned.max(self.available),
            });
            self.output.extend_from_slice(settings.request);
            self.next_ordinal += settings.connections;
            statistics.sent(&settings.timing, now);
            dispatched += 1;
        }
        dispatched
    }

    /// Whether queued bytes remain to be written.
    pub fn pending_output(&self) -> bool {
        self.written < self.output.len()
    }

    /// Write queued requests until they are written or the socket would block.
    pub fn flush(&mut self, statistics: &mut WorkerStatistics) -> Result<(), ErrorKind> {
        while self.pending_output() {
            let stream = self
                .stream
                .as_mut()
                .expect("only connected streams are written");
            match stream.write(&self.output[self.written..]) {
                Ok(0) => return Err(ErrorKind::Write),
                Ok(written) => {
                    self.written += written;
                    statistics.bytes_written += written as u64;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    statistics.write_would_block += 1;
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(classify(&error, ErrorKind::Write)),
            }
        }
        self.output.clear();
        self.written = 0;
        Ok(())
    }

    /// Read until the socket would block, completing every response that arrives. An error ends the connection: the caller charges it to the outstanding requests with [`Connection::fail`].
    pub fn readable(
        &mut self,
        clock: &impl Clock,
        settings: &Settings,
        statistics: &mut WorkerStatistics,
    ) -> Result<(), ErrorKind> {
        loop {
            if self.end == self.input.len() {
                self.make_room();
            }
            let stream = self
                .stream
                .as_mut()
                .expect("only connected streams are read");
            match stream.read(&mut self.input[self.end..]) {
                Ok(0) => return Err(self.end_of_stream(clock.now(), settings, statistics)),
                Ok(read) => {
                    self.end += read;
                    statistics.bytes_read += read as u64;
                    self.counters.bytes_read += read as u64;
                    self.process(clock.now(), settings, statistics)?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(classify(&error, ErrorKind::Read)),
            }
        }
    }

    /// Move unconsumed input to the front of the buffer, and grow it when it is full of one incomplete head, which the parser bounds.
    fn make_room(&mut self) {
        self.input.copy_within(self.start..self.end, 0);
        self.end -= self.start;
        self.start = 0;
        if self.end == self.input.len() {
            self.input.resize(self.input.len() * 2, 0);
        }
    }

    fn process(
        &mut self,
        now: u64,
        settings: &Settings,
        statistics: &mut WorkerStatistics,
    ) -> Result<(), ErrorKind> {
        while self.start < self.end {
            if self.in_flight.is_empty() {
                // Bytes that no request asked for.
                return Err(ErrorKind::Parse);
            }
            let (consumed, response) = self
                .parser
                .parse(
                    &self.input[self.start..self.end],
                    settings.expectation,
                    settings.head,
                )
                .map_err(|_| ErrorKind::Parse)?;
            self.start += consumed;
            let Some(response) = response else { break };
            let close = response.close;
            self.complete(now, response, settings, statistics);
            if close {
                return Err(ErrorKind::Closed);
            }
        }
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        }
        Ok(())
    }

    fn complete(
        &mut self,
        now: u64,
        response: Response,
        settings: &Settings,
        statistics: &mut WorkerStatistics,
    ) {
        let request = self
            .in_flight
            .pop_front()
            .expect("a response completes an outstanding request");
        if self.in_flight.len() + 1 == settings.depth {
            self.available = now;
        }
        statistics.complete(
            &settings.timing,
            &request,
            now,
            response.invalid,
            &mut self.counters,
        );
    }

    /// The server closed the connection: complete a response delimited by the close, then report the end of the stream. Requests pipelined behind a close-delimited response were ended by a response that closes, as behind `Connection: close`; otherwise the server closed before their responses were complete.
    fn end_of_stream(
        &mut self,
        now: u64,
        settings: &Settings,
        statistics: &mut WorkerStatistics,
    ) -> ErrorKind {
        if self.in_flight.is_empty() && self.start == self.end {
            statistics.idle_closes += 1;
        }
        match self.parser.at_eof(settings.expectation) {
            Some(response) => {
                self.complete(now, response, settings, statistics);
                ErrorKind::Closed
            }
            None => ErrorKind::Eof,
        }
    }

    /// Whether the oldest outstanding request has waited for its response longer than the timeout.
    pub fn timed_out(&self, now: u64, settings: &Settings) -> bool {
        self.in_flight
            .front()
            .is_some_and(|request| now.saturating_sub(request.sent) >= settings.timeout)
    }

    /// Close the connection after `kind` of failure, charge it to every outstanding request, and allow a reconnection after the delay.
    pub fn fail(
        &mut self,
        kind: ErrorKind,
        now: u64,
        settings: &Settings,
        statistics: &mut WorkerStatistics,
    ) {
        for request in self.in_flight.drain(..) {
            statistics.fail(&settings.timing, &request, now, kind, &mut self.counters);
        }
        self.stream = None;
        self.state = State::Down {
            until: now.saturating_add(settings.reconnect_delay),
        };
        self.output.clear();
        self.written = 0;
        self.start = 0;
        self.end = 0;
        self.parser = ResponseParser::default();
        statistics.disconnects += 1;
    }

    /// At the drain deadline: outstanding requests time out, and in the open loop, planned requests never sent are counted as unsent. The connection is closed for good.
    pub fn abandon(&mut self, now: u64, settings: &Settings, statistics: &mut WorkerStatistics) {
        if self.stream.is_some() {
            self.fail(ErrorKind::Timeout, now, settings, statistics);
        }
        if settings.schedule.is_some() {
            let warmup = count_below(
                self.next_ordinal,
                settings.connections,
                settings.measured_first,
            );
            let unsent = count_below(self.next_ordinal, settings.connections, settings.total);
            statistics.unsent(Period::Warmup, warmup);
            statistics.unsent(Period::Measured, unsent - warmup);
            self.next_ordinal = settings.total.max(self.next_ordinal);
        }
        self.state = State::Down { until: u64::MAX };
    }
}

/// The error kind of a failed read or write: a reset, or `default`.
fn classify(error: &io::Error, default: ErrorKind) -> ErrorKind {
    match error.kind() {
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionAborted => ErrorKind::Reset,
        _ => default,
    }
}

#[cfg(test)]
mod tests;
