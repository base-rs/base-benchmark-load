// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Where the controller and the workers may run, read from each thread's affinity rather than inferred from timing.

use super::{extension, load, record, server::Server};
use serde_json::json;
use std::time::{Duration, Instant};

/// The CPUs the calling thread may run on.
fn affinity() -> Vec<usize> {
    // SAFETY: `set` is a valid, initialized CPU set of the size passed, and `CPU_ISSET` stays within it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        assert_eq!(
            libc::sched_getaffinity(0, std::mem::size_of_val(&set), &mut set),
            0
        );
        (0..libc::CPU_SETSIZE as usize)
            .filter(|cpu| libc::CPU_ISSET(*cpu, &set))
            .collect()
    }
}

/// What a run on a thread of its own found of its placement.
struct Placed {
    /// The CPUs the thread could use before and after the run.
    before: Vec<usize>,
    after: Vec<usize>,
    /// The record's workers.
    workers: Vec<serde_json::Value>,
    /// How long the run took, measured around it.
    elapsed: Duration,
}

/// Run a short open loop with `extra` on a thread of its own, since the run pins the calling thread as its controller.
fn placed(extra: &[String]) -> Placed {
    let server = Server::hello();
    let extra = extra.to_vec();
    std::thread::spawn(move || {
        let before = affinity();
        let mut arguments = vec![
            "--rate",
            "1000",
            "--connections",
            "2",
            "--warmup",
            "0",
            "--duration",
            "10ms",
        ];
        arguments.extend(extra.iter().map(String::as_str));
        let started = Instant::now();
        let report = load("rate", server.address, &arguments).unwrap();
        let elapsed = started.elapsed();
        let record = record(&report);
        let workers = extension(&record)["workers"].as_array().unwrap().clone();
        Placed {
            before,
            after: affinity(),
            workers,
            elapsed,
        }
    })
    .join()
    .unwrap()
}

/// Threads inherit their creator's affinity, so a controller pinned before its workers started would confine them all to its CPU.
#[test]
fn a_pinned_controller_leaves_unpinned_workers_where_they_were() {
    let controller = affinity()[0];
    let placed = placed(&[
        "--workers".to_owned(),
        "2".to_owned(),
        "--controller-cpu".to_owned(),
        controller.to_string(),
    ]);
    assert_eq!(placed.after, [controller]);
    assert_eq!(placed.workers.len(), 2);
    for worker in placed.workers {
        assert_eq!(worker["cpu"], serde_json::Value::Null);
        assert_eq!(worker["affinity"], json!(placed.before));
        // An unpinned worker has no CPU of its own whose software interrupt time it could report.
        assert_eq!(worker["softirq_ns"], serde_json::Value::Null);
    }
}

#[test]
fn pinned_workers_run_only_on_their_cpus() {
    let allowed = affinity();
    let worker = *allowed.last().unwrap();
    let mut extra = vec!["--cpus".to_owned(), worker.to_string()];
    // With a second CPU, the controller is pinned apart from the worker.
    let controller = allowed.first().filter(|cpu| **cpu != worker).copied();
    if let Some(cpu) = controller {
        extra.extend(["--controller-cpu".to_owned(), cpu.to_string()]);
    }
    let placed = placed(&extra);
    assert_eq!(
        placed.after,
        controller.map_or(placed.before, |cpu| vec![cpu])
    );
    assert_eq!(placed.workers.len(), 1);
    assert_eq!(placed.workers[0]["cpu"], worker);
    assert_eq!(placed.workers[0]["affinity"], json!([worker]));
    // A pinned worker reports its CPU's software interrupt time over the controller's window, which lies within the run: no more than the run took, and one tick of `/proc/stat`, since both ends of the window are read in whole ticks. Its CPU's time since boot would exceed it.
    // SAFETY: `sysconf` has no preconditions.
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let tick = Duration::from_secs(1) / u32::try_from(ticks_per_second).unwrap();
    let softirq = placed.workers[0]["softirq_ns"]
        .as_u64()
        .unwrap_or_else(|| panic!("{}", placed.workers[0]));
    assert!(
        u128::from(softirq) <= (placed.elapsed + tick).as_nanos(),
        "{softirq} ns in software interrupts over a run of {:?}",
        placed.elapsed
    );
}
