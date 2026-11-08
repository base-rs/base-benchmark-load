// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A small threaded HTTP/1.1 server whose behaviour each test chooses per request.

use std::{
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

pub const HELLO: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nHello, world!";

/// What the server does with one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Respond(Vec<u8>),
    /// Wait, then respond.
    Delay(Duration, Vec<u8>),
    /// Respond, then close the connection.
    RespondAndClose(Vec<u8>),
    /// Read the request and never respond.
    Ignore,
    /// Close the connection without responding.
    Close,
    /// Reset the connection.
    Reset,
}

/// Which request a behaviour is deciding about: the connection's accept order, from zero, which is the preflight connection, and the request's order on its connection.
#[derive(Clone, Copy, Debug)]
pub struct Arrival {
    pub connection: usize,
    pub request: usize,
}

type Behaviour = Arc<dyn Fn(Arrival) -> Action + Send + Sync>;

pub struct Server {
    pub address: SocketAddr,
    stop: Arc<AtomicBool>,
    pub requests: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(behaviour: impl Fn(Arrival) -> Action + Send + Sync + 'static) -> Self {
        Self::with_accept_limit(usize::MAX, behaviour)
    }

    /// A server that stops listening once it accepts its `limit`th connection, before serving it, so that every later connection is refused.
    pub fn with_accept_limit(
        limit: usize,
        behaviour: impl Fn(Arrival) -> Action + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let behaviour: Behaviour = Arc::new(behaviour);
        let accept = {
            let (stop, requests) = (stop.clone(), requests.clone());
            std::thread::spawn(move || {
                let mut listener = Some(listener);
                for connection in 0..limit {
                    let Ok((stream, _)) = listener.as_ref().unwrap().accept() else {
                        break;
                    };
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    if connection + 1 == limit {
                        // Stop listening before serving the last connection, so that any connection attempted after its first response is refused. Shut the socket down rather than only closing it: a process spawned concurrently by another test may briefly hold a duplicate of the descriptor, which would keep it listening.
                        let listening = listener.take().unwrap();
                        // SAFETY: The descriptor is open.
                        unsafe { libc::shutdown(listening.as_raw_fd(), libc::SHUT_RDWR) };
                    }
                    let (behaviour, requests) = (behaviour.clone(), requests.clone());
                    std::thread::spawn(move || serve(stream, connection, &*behaviour, &requests));
                }
            })
        };
        Self {
            address,
            stop,
            requests,
            accept: Some(accept),
        }
    }

    pub fn hello() -> Self {
        Self::start(|_| Action::Respond(HELLO.to_vec()))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the accept loop, if it is still accepting.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        let _ = self.accept.take().unwrap().join();
    }
}

fn serve(
    mut stream: TcpStream,
    connection: usize,
    behaviour: &(dyn Fn(Arrival) -> Action + Send + Sync),
    requests: &AtomicUsize,
) {
    let mut buffer = Vec::new();
    let mut chunk = [0; 16 * 1024];
    let mut request = 0;
    loop {
        let Ok(read) = stream.read(&mut chunk) else {
            return;
        };
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        while let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if buffer.len() < end + 4 + length {
                break;
            }
            buffer.drain(..end + 4 + length);
            requests.fetch_add(1, Ordering::Relaxed);
            let arrival = Arrival {
                connection,
                request,
            };
            request += 1;
            let written = match behaviour(arrival) {
                Action::Respond(bytes) => stream.write_all(&bytes),
                Action::Delay(delay, bytes) => {
                    std::thread::sleep(delay);
                    stream.write_all(&bytes)
                }
                Action::RespondAndClose(bytes) => {
                    let _ = stream.write_all(&bytes);
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
                Action::Ignore => Ok(()),
                Action::Close => {
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
                Action::Reset => {
                    reset(&stream);
                    return;
                }
            };
            if written.is_err() {
                return;
            }
        }
    }
}

/// Close with a reset rather than a FIN: a zero linger time discards unsent data and sends RST.
fn reset(stream: &TcpStream) {
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: The descriptor is open and `linger` is a valid option value of its size.
    unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&raw const linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
}

/// A loopback address that refuses connections: a socket bound to a free port that never listens, which also keeps the port from being reused while it lives.
pub fn refusing() -> (OwnedFd, SocketAddr) {
    // SAFETY: `socket` returns a new descriptor, owned by nothing else, which is checked below.
    let socket = unsafe {
        OwnedFd::from_raw_fd(libc::socket(
            libc::AF_INET,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
        ))
    };
    let mut inet = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(std::net::Ipv4Addr::LOCALHOST).to_be(),
        },
        sin_zero: [0; 8],
    };
    let mut length = size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: `inet` is a valid IPv4 socket address of `length` bytes, for both calls.
    unsafe {
        assert_eq!(
            libc::bind(
                std::os::fd::AsRawFd::as_raw_fd(&socket),
                (&raw const inet).cast(),
                length
            ),
            0
        );
        assert_eq!(
            libc::getsockname(
                std::os::fd::AsRawFd::as_raw_fd(&socket),
                (&raw mut inet).cast(),
                &mut length
            ),
            0
        );
    }
    (
        socket,
        SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, u16::from_be(inet.sin_port))),
    )
}
