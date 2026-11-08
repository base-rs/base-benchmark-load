// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use std::{io::Read, io::Write, net::TcpListener};

#[test]
fn results_carry_errno() {
    assert_eq!(result(3).unwrap(), 3);
    // SAFETY: Closing an invalid descriptor fails with EBADF and has no other effect.
    let error = result(unsafe { libc::close(-1) }).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EBADF));
}

#[test]
fn pinning_reports_the_cpu() {
    std::thread::spawn(|| {
        // The calling thread can always run on a CPU it is already allowed to use.
        // SAFETY: `sched_getcpu` has no preconditions.
        let cpu = unsafe { libc::sched_getcpu() } as usize;
        assert!(affinity().contains(&cpu));
        pin(cpu).unwrap();
        assert_eq!(affinity(), [cpu]);
        // CPU_SETSIZE - 1 is offline on any machine these tests run on.
        let error = pin(libc::CPU_SETSIZE as usize - 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("pinning to CPU 1023"), "{error}");
    })
    .join()
    .unwrap();
}

#[test]
fn thread_cpu_time_advances_with_work() {
    let before = thread_cpu_time();
    let mut value = 0u64;
    while thread_cpu_time() == before {
        value = std::hint::black_box(value.wrapping_add(1));
    }
    assert!(thread_cpu_time() > before);
}

#[test]
fn the_descriptor_limit_is_raised_within_the_hard_limit() {
    let current = raise_descriptor_limit(0);
    assert_eq!(raise_descriptor_limit(current), current);
    let raised = raise_descriptor_limit(current + 1);
    assert!(raised >= current);
    assert!(raise_descriptor_limit(u64::MAX) >= raised);
}

#[test]
fn connects_without_blocking() {
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let Ok(listener) = TcpListener::bind(address) else {
            // IPv6 may be disabled on the host.
            continue;
        };
        let mut stream = connect(listener.local_addr().unwrap()).unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        stream.set_nonblocking(false).unwrap();
        assert!(stream.nodelay().unwrap());
        stream.write_all(b"ping").unwrap();
        let mut buffer = [0; 4];
        accepted.read_exact(&mut buffer).unwrap();
        assert_eq!(&buffer, b"ping");
        assert!(stream.take_error().unwrap().is_none());
    }
}

#[test]
fn connection_failures_are_reported() {
    // A loopback port that nothing listens on: the refusal arrives immediately or as the socket's error.
    let (_socket, address) = refusing();
    match connect(address) {
        Ok(stream) => {
            stream.set_nonblocking(false).unwrap();
            let mut buffer = [0; 1];
            assert!((&stream).read(&mut buffer).is_err() || stream.take_error().unwrap().is_some());
        }
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused),
    }
    // TCP cannot connect to a multicast address, which Linux rejects at once.
    let error = connect("224.0.0.1:80".parse().unwrap()).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENETUNREACH));
}

#[test]
fn the_clock_source_is_named() {
    if let Some(source) = clock_source() {
        assert!(!source.is_empty());
        assert!(!source.ends_with('\n'));
    }
}
