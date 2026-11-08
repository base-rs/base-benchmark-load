// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The Linux calls the standard library does not offer: CPU affinity, thread CPU time, the descriptor limit, non-blocking connects, and each CPU's software interrupt time.

use std::{
    collections::BTreeMap,
    io,
    net::{SocketAddr, TcpStream},
    os::fd::{FromRawFd, OwnedFd},
};

/// Convert a C return value, where -1 means failure with `errno` set.
pub fn result(value: libc::c_int) -> io::Result<libc::c_int> {
    if value == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

/// Run the calling thread only on `cpu`, which is below `CPU_SETSIZE`.
pub fn pin(cpu: usize) -> io::Result<()> {
    // SAFETY: `set` is a valid, initialized CPU set for the calling thread, and the options reject CPUs at or beyond `CPU_SETSIZE`, so `CPU_SET` stays within it.
    result(unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set)
    })
    .map(drop)
    .map_err(|error| io::Error::new(error.kind(), format!("pinning to CPU {cpu}: {error}")))
}

/// The CPUs the calling thread may run on, in increasing order.
pub fn affinity() -> Vec<usize> {
    // SAFETY: `set` is a valid, initialized CPU set of the size passed. Querying the calling thread with a set of `CPU_SETSIZE` CPUs cannot fail, and `CPU_ISSET` stays within the set.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::sched_getaffinity(0, std::mem::size_of_val(&set), &mut set);
        (0..libc::CPU_SETSIZE as usize)
            .filter(|cpu| libc::CPU_ISSET(*cpu, &set))
            .collect()
    }
}

/// The calling thread's CPU time in nanoseconds.
pub fn thread_cpu_time() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is valid for writes. The thread CPU clock always exists, so the call cannot fail.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

/// Raise the soft limit on open descriptors to at least `needed`, up to the hard limit. Returns the soft limit in effect afterwards, read back from the kernel, which may be below `needed`.
pub fn raise_descriptor_limit(needed: u64) -> u64 {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is valid for reads and writes. `getrlimit` fails only for an invalid resource or pointer. If `setrlimit` fails, the limit is unchanged, and reading it back reports that.
    unsafe {
        libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit);
        if limit.rlim_cur < needed {
            limit.rlim_cur = needed.min(limit.rlim_max);
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
            libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit);
        }
    }
    limit.rlim_cur
}

/// Start a non-blocking TCP connection to `address`. The connection completes when the socket becomes writable; check [`TcpStream::take_error`] then.
pub fn connect(address: SocketAddr) -> io::Result<TcpStream> {
    // SAFETY: `socket` returns a new descriptor or -1.
    result(unsafe {
        libc::socket(
            if address.is_ipv4() {
                libc::AF_INET
            } else {
                libc::AF_INET6
            },
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    })
    .and_then(|descriptor| {
        // SAFETY: A new descriptor is owned by nothing else.
        let socket = TcpStream::from(unsafe { OwnedFd::from_raw_fd(descriptor) });
        socket
            .set_nodelay(true)
            .expect("TCP sockets accept TCP_NODELAY");
        let (storage, length) = socket_address(address);
        // SAFETY: `storage` holds a socket address of `length` bytes, and the descriptor is open.
        let connected =
            result(unsafe { libc::connect(descriptor, (&raw const storage).cast(), length) });
        match connected {
            Err(error) if error.raw_os_error() != Some(libc::EINPROGRESS) => Err(error),
            _ => Ok(socket),
        }
    })
}

fn socket_address(address: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: An all-zero `sockaddr_storage` is valid, and it is large and aligned enough for either address family.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let length = match address {
        SocketAddr::V4(address) => {
            let inet = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: address.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(*address.ip()).to_be(),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: As above.
            unsafe { (&raw mut storage).cast::<libc::sockaddr_in>().write(inet) };
            std::mem::size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(address) => {
            let inet = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: address.port().to_be(),
                sin6_flowinfo: address.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: address.ip().octets(),
                },
                sin6_scope_id: address.scope_id(),
            };
            // SAFETY: As above.
            unsafe { (&raw mut storage).cast::<libc::sockaddr_in6>().write(inet) };
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, length as libc::socklen_t)
}

/// The kernel's current clock source, such as `tsc`, which decides how cheap and precise reading the clock is.
pub fn clock_source() -> Option<String> {
    std::fs::read_to_string("/sys/devices/system/clocksource/clocksource0/current_clocksource")
        .ok()
        .map(|source| source.trim().to_owned())
}

/// Each online CPU's time in software interrupts since boot, in nanoseconds, by CPU number, from `/proc/stat`.
pub fn softirq_times() -> io::Result<BTreeMap<usize, u64>> {
    // SAFETY: `sysconf` has no preconditions; `_SC_CLK_TCK` is always defined on Linux.
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    std::fs::read_to_string("/proc/stat").map(|stat| softirq_from_stat(&stat, ticks_per_second))
}

/// Each CPU's software interrupt time in `stat`, the text of `/proc/stat`, whose times count `ticks_per_second` ticks a second. The `softirq` field is the seventh of a `cpuN` line, after `user`, `nice`, `system`, `idle`, `iowait`, and `irq`.
pub(crate) fn softirq_from_stat(stat: &str, ticks_per_second: u64) -> BTreeMap<usize, u64> {
    stat.lines()
        .filter_map(|line| {
            let mut fields = line.split_ascii_whitespace();
            let cpu = fields.next()?.strip_prefix("cpu")?.parse().ok()?;
            let ticks: u64 = fields.nth(6)?.parse().ok()?;
            Some((cpu, ticks.saturating_mul(1_000_000_000) / ticks_per_second))
        })
        .collect()
}

#[cfg(test)]
/// A loopback address that refuses connections: a socket bound to a free port that never listens, which also keeps the port from being reused while it lives.
pub fn refusing() -> (OwnedFd, SocketAddr) {
    // SAFETY: `socket` returns a new descriptor or -1.
    let descriptor =
        result(unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) })
            .expect("creating a socket succeeds");
    // SAFETY: A new descriptor is owned by nothing else.
    let socket = unsafe { OwnedFd::from_raw_fd(descriptor) };
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

#[cfg(test)]
mod tests;
