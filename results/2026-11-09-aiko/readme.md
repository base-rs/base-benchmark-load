# Saturation and read changes, screened on aiko, 9 November 2026

This screen checks the changes that followed [the hana evaluation](../2026-11-09-hana/readme.md): schedule lag, the shortfall reason, the 0.8 busy limit, and one `recvfrom` per response. It is **screening evidence**, not a recorded measurement: aiko is a shared desktop, and during the sweep another session's test process, free to run on any CPU, moved onto the measured CPUs, and the load average rose from 6 to 107. Most rounds above 400k requests per second were disturbed for every client, including the reference, which failed 6 of its 35 runs when its backlog did not drain within its 15 s limit, so the sweep shows how each build's saturation reasons respond to disturbed runs, and does not measure capacity.

## Conditions

- Host aiko, AMD Ryzen 9 9950X3D, Linux 7.2.14-arch1-1, `powersave` governor, Rust 1.99.1, `cargo build --release --locked`. The server on CPUs 2 and 3, the client's worker on CPU 4 and its controller on CPU 6 (the fixed-rate client's controller on CPU 6 with `taskset`), with their hyperthread siblings left to the rest of the host. Every sample started a fresh server and held the host lock `/tmp/base-benchmark-host.lock`.
- `before` is `base-benchmark-load` at 30212f8. `after` is 30212f8 with [after.patch](after.patch) applied to `src` and `tests`, which rebuilds the measured binary exactly; it differs from the pull request's revision in judging the schedule lag on the workers' merged distribution rather than each worker's, the same check with one worker, and in the help line and documentation of the defaults. `fixed` is the hello benchmark's fixed-rate client, and the server the hello benchmark's `base-iouring-raw`, both from `base-http` 8118a0a, as on hana. [binaries.sha256](binaries.sha256) has their digests; `before` is identical to the binary measured on hana.
- `GET /` with a 13-byte body, 64 connections, depth 1, one worker. The sweep ran 5 rounds at 400k–700k requests per second with a 1 s warmup and 4 s window, in a shuffled order within each round; the pilots ran 1 round at 100k–350k and 400k–700k.

## Files

| File | What it holds |
| --- | --- |
| `strace/` | `strace -f -c` counts of each client at 20,000 requests per second for 3 s (`scripts/measure.py strace`). |
| `samples/` | One row per sample, with the load average before it and a summary of the client's output (`scripts/measure.py sweep`). |
| [tables.md](tables.md) | The sweep's medians, with ranges, and the reasons for each `after` flag (`scripts/table.py samples/sweep.jsonl`). |
| [after.patch](after.patch) | The `after` source as measured, against 30212f8. |
| [binaries.sha256](binaries.sha256) | The measured binaries' digests. |
| [flags.txt](flags.txt) | Each build's flags against disturbed and undisturbed runs, over the pilots and the sweep (`scripts/flags.py samples/*.jsonl`). |

## Results

**One `recvfrom` per response.** At 20,000 requests per second for 3 s, 60,000 responses:

| Client | `recvfrom` calls | of which `EAGAIN` | `sendto` calls |
| --- | ---: | ---: | ---: |
| before | 120,001 | 60,000 | 60,001 |
| after | 60,001 | 0 | 60,001 |
| fixed-rate client | 60,000 | 0 | 60,000 |

Each client's extra call is the preflight request's.

**Saturation reasons.** Over the 35 runs of each build, counting a run as disturbed when its 99th percentile latency exceeded 1 ms and as undisturbed when it was at most 100 µs:

| Build | Disturbed | not flagged | Undisturbed | flagged |
| --- | ---: | ---: | ---: | ---: |
| before (busy over 0.9) | 20 | 11 | 12 | 1 |
| after (busy over 0.8, schedule lag, shortfall) | 21 | 0 | 11 | 6 |

The runs `before` missed had a busy fraction of 0.55 to 0.88 while their worker was preempted or overloaded, with up to 2.4 million planned requests unsent in the window. In `after`, the schedule lag or the shortfall flagged every such run; at 100k, an `after` run with its worker busy for 28% of the window had a 3.1 ms 99th percentile schedule lag, from a worker that could not run. Undisturbed, the 99th percentile schedule lag was 0.4–7.5 µs.

Five of the six undisturbed runs `after` flagged were flagged only for a busy fraction of 0.82 to 0.85: four at 500k, with 99th percentiles of 7.6–11.8 µs, within the 6.7–36.4 µs of the fixed-rate client's undisturbed runs there, and one at 400k with 42.1 µs, against its 6.8–10.1 µs. On this host, faster than hana, the 0.8 busy limit is conservative. The sixth was busy for 97% of the window.

## Not measured

Capacity: whether one `recvfrom` per response raises the rate one worker sustains could not be resolved on a host this disturbed. Hardware counters, more than one worker, and depths above 1.
