# Window markers under the runner, on aiko, 9 November 2026

This checks `base-benchmark-load` as the driver role of a `base-benchmark` 0.4.0 multi-process scenario, run by the real runner rather than the tests' fake one, and measures how long the runner takes to acknowledge the window's start, which the default `--window-lead` must allow for. It is **screening evidence**: aiko is a shared desktop, and other sessions kept its load average between 12 and 99 on its 32 CPUs throughout.

## Conditions

- Host aiko, AMD Ryzen 9 9950X3D (16 cores, 32 threads), Linux 7.2.14-arch1-1, Rust 1.99.1. `base-benchmark` 0.4.0 from crates.io, installed with `cargo install --locked`; `base-benchmark-load` built with `cargo build --release` from this pull request's source, given to the runner with `--program load=PATH`.
- The [scenario](scenario/Cargo.toml) runs a [fixture server](scenario/src/main.rs), a thread per connection answering every request with a 13-byte body, as the subject service, ready on `listening on {address}`, and the load client as the driver with `invalid-responses = "responses_invalid"`: rate mode at 20,000 requests per second over 16 connections, one worker on CPU 2, with a 1 s warmup and a 2 s window. `--role-cpus client=2,3 --role-cpus server=4,5`, so the controller ran on CPU 3. Each campaign was `base-benchmark run hello --rounds 30` in screening mode, under the host lock.
- `lead-500ms` added `--window-lead 500ms` to the client's arguments; `lead-100ms` used the default this pull request sets, 100 ms. Before either, the same scenario ran with the then default of 10 ms.

## Files

| File | What it holds |
| --- | --- |
| `samples/` | One row per sample of each campaign: the record's `window` extension, its operations, and the server's window CPU time from the runner's journal (`scripts/extract.py`). |
| `scenario/` | The scenario's package, as for `lead-100ms`, with the server formatted and given its license header since. |

## Results

| Campaign | Samples | Failed | Acknowledgement | Controller's stop after the window |
| --- | ---: | ---: | --- | --- |
| 10 ms lead | 2 | 1 | 1.854 ms in the one that passed | 4.430 ms in the one that passed |
| `lead-500ms` | 30 | 0 | 37 µs to 3.303 ms, median 106 µs | 52 µs to 3.889 ms |
| `lead-100ms` | 30 | 0 | 44 µs to 160 µs, median 90 µs | 55 µs to 1.774 ms |

The acknowledgement is the time from writing the start marker to reading the runner's reply. With the 10 ms lead, the second sample's start was acknowledged 5.083 ms after its window began, so the run failed, as it must, and the campaign stopped; its samples were not kept, and the table gives what the runner journalled. Either the runner or the controller, which also woke up to 3.9 ms late at the window's end, can lose that much time on a busy host. With 100 ms, every start was acknowledged at least 98.35 ms before its window.

Every sample of the two 30-round campaigns has 40,000 operations, the window's valid responses, and the runner reported the server's window CPU per request as `server/cpu_time_per_operation`, marked approximate: with the default lead, a median of 1.59 µs, from 1.37 to 5.43 µs across the disturbed rounds, computed here from the journal's window CPU time over the operations. The runner found `responses_invalid` as a total in every record.

## Conclusion

A 10 ms lead is too short for screening on a busy host, where a late acknowledgement fails the campaign. The default of 100 ms, the same lead as the load's start has over the workers, left a margin of at least 98 ms here, and takes at most 100 ms of the warmup's load into the services' window: 5% of this 2-second window, and 1.25% of an 8-second one, at the same offered rate as the window's, so it inflates CPU per request alike for every subject. On an idle measuring host, a shorter lead makes the edge tighter; a lead at least as long as the warmup never fails, but takes in the whole warmup.
