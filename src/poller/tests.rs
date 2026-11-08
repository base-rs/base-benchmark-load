// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use std::{
    io::Write,
    net::{TcpListener, TcpStream},
};

#[test]
fn reports_edge_triggered_readiness() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let mut epoll = Epoll::new().unwrap();
    epoll.add(client.as_raw_fd(), 7).unwrap();
    let mut events = [Event::default(); 4];
    // A connected socket is writable at once.
    assert_eq!(epoll.wait(&mut events, 1000).unwrap(), 1);
    assert_eq!(
        events[0],
        Event {
            token: 7,
            readable: false,
            writable: true,
            closed: false
        }
    );
    // Edge-triggered: nothing new until the state changes.
    assert_eq!(epoll.wait(&mut events, 0).unwrap(), 0);
    server.write_all(b"x").unwrap();
    assert_eq!(epoll.wait(&mut events, 1000).unwrap(), 1);
    assert!(events[0].readable);
    drop(server);
    assert_eq!(epoll.wait(&mut events, 1000).unwrap(), 1);
    assert!(events[0].closed);
}

#[test]
fn registrations_outlive_a_close_while_a_duplicate_exists_unless_deleted() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut epoll = Epoll::new().unwrap();
    let mut events = [Event::default(); 4];
    for delete in [false, true] {
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        epoll.add(client.as_raw_fd(), 1).unwrap();
        assert_eq!(epoll.wait(&mut events, 1000).unwrap(), 1);
        // A duplicate, as a child holds between `fork` and `exec`.
        let duplicate = client.try_clone().unwrap();
        if delete {
            epoll.delete(client.as_raw_fd()).unwrap();
        }
        drop(client);
        server.write_all(b"x").unwrap();
        let reported = epoll
            .wait(&mut events, if delete { 0 } else { 1000 })
            .unwrap();
        assert_eq!(reported, usize::from(!delete), "deleted: {delete}");
        drop(duplicate);
    }
}

#[test]
fn registration_errors_are_reported() {
    let mut epoll = Epoll::new().unwrap();
    let error = epoll.add(-1, 0).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EBADF));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let error = epoll.delete(listener.as_raw_fd()).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn interrupted_waits_find_no_events() {
    assert_eq!(interrupted(io::ErrorKind::Interrupted.into()).unwrap(), 0);
    assert_eq!(
        interrupted(io::ErrorKind::Other.into()).unwrap_err().kind(),
        io::ErrorKind::Other
    );
}
