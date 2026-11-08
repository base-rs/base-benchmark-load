// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Where the controller and the workers may run, read from each thread's affinity rather than inferred from timing.

use super::{extension, load, record, server::Server};
use serde_json::json;

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

/// Run a short open loop with `extra` on a thread of its own, since the run pins the calling thread as its controller. Returns the CPUs that thread could use before and after, and the workers' affinities.
fn placed(extra: &[String]) -> (Vec<usize>, Vec<usize>, Vec<serde_json::Value>) {
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
        let record = record(&load("rate", server.address, &arguments).unwrap());
        let workers = extension(&record)["workers"].as_array().unwrap().clone();
        (before, affinity(), workers)
    })
    .join()
    .unwrap()
}

/// Threads inherit their creator's affinity, so a controller pinned before its workers started would confine them all to its CPU.
#[test]
fn a_pinned_controller_leaves_unpinned_workers_where_they_were() {
    let controller = affinity()[0];
    let (before, after, workers) = placed(&[
        "--workers".to_owned(),
        "2".to_owned(),
        "--controller-cpu".to_owned(),
        controller.to_string(),
    ]);
    assert_eq!(after, [controller]);
    assert_eq!(workers.len(), 2);
    for worker in workers {
        assert_eq!(worker["cpu"], serde_json::Value::Null);
        assert_eq!(worker["affinity"], json!(before));
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
    let (before, after, workers) = placed(&extra);
    assert_eq!(after, controller.map_or(before, |cpu| vec![cpu]));
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0]["cpu"], worker);
    assert_eq!(workers[0]["affinity"], json!([worker]));
}
