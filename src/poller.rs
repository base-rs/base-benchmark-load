// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::os;
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

/// Readiness of one registered descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub token: u64,
    pub readable: bool,
    pub writable: bool,
    /// The peer closed, or the socket has an error; reading or checking the error says which.
    pub closed: bool,
}

/// The readiness interface workers poll. Registrations are edge-triggered for reading, writing, and closing, so a worker must read and write until the socket would block.
///
/// Workers busy-poll it with a zero timeout. It is deliberately thin, so another backend, such as io_uring, can replace epoll without changing the workers.
pub trait Poller {
    fn add(&mut self, descriptor: RawFd, token: u64) -> io::Result<()>;

    /// Remove a registration, before its descriptor is closed. Closing alone is not enough: while another process holds a duplicate of the descriptor, as a child does between `fork` and `exec`, the registration and its events outlive the close.
    fn delete(&mut self, descriptor: RawFd) -> io::Result<()>;

    /// Wait at most `timeout` milliseconds, zero to return immediately, for events, and write them to the start of `events`. Returns how many there are.
    fn wait(&mut self, events: &mut [Event], timeout: i32) -> io::Result<usize>;
}

/// An epoll instance.
#[derive(Debug)]
pub struct Epoll {
    descriptor: OwnedFd,
    buffer: Vec<libc::epoll_event>,
}

impl Epoll {
    pub fn new() -> io::Result<Self> {
        // SAFETY: `epoll_create1` returns a new descriptor or -1.
        os::result(unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) }).map(|descriptor| Self {
            // SAFETY: A new descriptor is owned by nothing else.
            descriptor: unsafe { OwnedFd::from_raw_fd(descriptor) },
            buffer: Vec::new(),
        })
    }
}

impl Poller for Epoll {
    fn add(&mut self, descriptor: RawFd, token: u64) -> io::Result<()> {
        let mut event = libc::epoll_event {
            events: (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP | libc::EPOLLET) as u32,
            u64: token,
        };
        // SAFETY: The epoll descriptor is open and `event` is valid; the kernel copies it before returning.
        os::result(unsafe {
            libc::epoll_ctl(
                self.descriptor.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                descriptor,
                &mut event,
            )
        })
        .map(drop)
    }

    fn delete(&mut self, descriptor: RawFd) -> io::Result<()> {
        // SAFETY: The epoll descriptor is open; a null event is allowed for deletion since Linux 2.6.9.
        os::result(unsafe {
            libc::epoll_ctl(
                self.descriptor.as_raw_fd(),
                libc::EPOLL_CTL_DEL,
                descriptor,
                std::ptr::null_mut(),
            )
        })
        .map(drop)
    }

    fn wait(&mut self, events: &mut [Event], timeout: i32) -> io::Result<usize> {
        self.buffer
            .resize(events.len(), libc::epoll_event { events: 0, u64: 0 });
        // SAFETY: The buffer is valid for writes of the advertised number of events, and the descriptor is open.
        os::result(unsafe {
            libc::epoll_wait(
                self.descriptor.as_raw_fd(),
                self.buffer.as_mut_ptr(),
                self.buffer.len() as i32,
                timeout,
            )
        })
        .map(|count| {
            let count = count as usize;
            for (event, raw) in events.iter_mut().zip(&self.buffer[..count]) {
                let flags = raw.events as i32;
                *event = Event {
                    token: raw.u64,
                    readable: flags & libc::EPOLLIN != 0,
                    writable: flags & libc::EPOLLOUT != 0,
                    closed: flags & (libc::EPOLLRDHUP | libc::EPOLLHUP | libc::EPOLLERR) != 0,
                };
            }
            count
        })
    }
}

/// Treat an interrupted wait as a wait that found no events.
pub fn interrupted(error: io::Error) -> io::Result<usize> {
    if error.kind() == io::ErrorKind::Interrupted {
        Ok(0)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests;
