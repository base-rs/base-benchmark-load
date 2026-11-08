# Evaluation on hana, 9 November 2026

This evaluation compared `base-benchmark-load` at 30212f8 with the reference clients it replaces, wrk for the closed loop and the hello benchmark's fixed-rate client for the open loop, against the hello benchmark's servers. It is **screening evidence**, not a recorded measurement: the host was not idle, the client's controller shared the worker's CPU, and the rounds are few. Its findings led to the saturation and read changes that follow it, and to the defaults of `--busy-limit`, `--schedule-lag-limit`, and `--shortfall-limit`; [the aiko screen](../2026-11-09-aiko/readme.md) checks those changes.

## Conditions

- Host hana, Linux 7.2.14-arch1-1, x86_64, `powersave` governor. The CPU model was not recorded. The host was not idle: its load average was 1.1 to 1.7, from media transcoding on other CPUs.
- The server on CPUs 2 and 3, the client on CPU 1 (`taskset -c 1`, `--cpus 1`, the controller unpinned within it), over IPv4 loopback. Every sample started a fresh server and held the host lock `/tmp/base-benchmark-host.lock`.
- Binaries built with `cargo build --release --locked` from `git archive` sources: `base-benchmark-load` 30212f8, `base-http` 8118a0a (the hello benchmark server and its fixed-rate client), and wrk 4.1.0-22-g556f8db; [binaries.sha256](binaries.sha256) has their digests.
- Workloads: `GET /` with a 13-byte `Hello, world!` response, 64 connections unless stated, depth 1. Closed loop: 5 rounds, 2 s warmup, 8 s window. Open loop: 5 rounds at 100k–200k requests per second with a 2 s warmup and 8 s window; a saturation sweep of 3 rounds at 120k–230k with a 1 s warmup and 4 s window; and a one-round sweep at 100k–240k.
- Server CPU is from `/proc/PID/stat` over the client's run, and softirq from `/proc/stat` for CPUs 2 and 3.

## Files

| File | What it holds |
| --- | --- |
| [tables.md](tables.md) | The closed-loop, open-loop, and saturation tables: medians, with the range in parentheses. `scripts/tables.py raw` regenerates it exactly. |
| `raw/EXPERIMENT/samples.jsonl` | One row per sample: the client's summary (from its record, or the fixed-rate client's JSON, or wrk's output), the server's CPU time, the client's and server's CPUs' times from `/proc/stat`, and the load average. `environment.json` has the driver's arguments and the host. `scripts/trim.py` reduced the raw output to these, leaving out the 30 MB of per-sample client outputs. |
| [raw/stall/summary.json](raw/stall/summary.json), [raw/corrupt/summary.json](raw/corrupt/summary.json) | `validate.py`'s healthy, stalled, and corrupted cases for both open-loop clients, and further corrupted cases for `base-benchmark-load`, from `scripts/stall.py` and `scripts/stall.py corrupt`. |
| `raw/strace/` | `strace -f -c` counts of each open-loop client at 20,000 requests per second for 3 s. |
| [record-check.jsonl](record-check.jsonl), [record-semantics.json](record-semantics.json) | Every record parsed and validated with `base-benchmark` 0.2.1 and 0.3.0, and semantic checks of the records with the server's own request counts, from `scripts/check_records.py` over the full raw output. |
| `scripts/` | The drivers, as run from `~/.cache/load-eval-20261109` on hana. |

## Results

**Records.** Every record validated with `base-benchmark` 0.2.1 and 0.3.0 and passed the semantic checks, with exact cohorts: every planned request has one outcome, and the server's request count is the sum of the outcomes and the preflight request. Parameters were equal across servers on different ports. A 40 ms stall stayed in the latency and send delay of both clients, with and without pipelining, before or within the window. A server corrupting every response, the preflight's included, made the fixed-rate client fail; `base-benchmark-load` took the preflight's corrupted body as the expected one and counted every response valid, as it is documented to. Given the body, or the whole response, its preflight failed instead, and one corrupted response among valid ones was counted invalid.

**Closed loop.** `base-benchmark-load` drove more throughput than wrk at every connection count, by medians of 9–35% paired by round (5.5–36.7% across rounds), and the server spent less CPU time per request under it.

**Open loop.** At 100k requests per second, its latency quantiles were within 1.15 times the fixed-rate client's. Above that, it fell behind the reference on one core:

| Offered | Busy | p99 vs fixed-rate client | p99.9 vs fixed-rate client | Flagged |
| ---: | ---: | ---: | ---: | ---: |
| 120k | 0.76–0.77 | 49.6 vs 44.8 µs (1.1×) | 232 vs 1,118 µs, noise | 0/3 |
| 140k | 0.86–0.88 | 73.9 vs 54.7 µs (1.35×); 63.7 vs 50.0 µs (1.27×) | 2.3×; 3.0× | 0/10 |
| 150k | 0.94 | 103 vs 61 µs | 6.4× | 3/3 |
| 230k | 1.00 | 536 ms vs 3.1 ms | | 3/3 |

Medians, with base-iouring-raw, and at 140k tokio-task-raw after the semicolon; 120k, 150k, and 230k are from the saturation sweep. The fixed-rate client sustained 230k; `base-benchmark-load` reached about 207k.

## Findings

1. **Dispatch lag collapses under overload.** With depth 1, a response the worker read late counted as its connection having had no room, so the delay never reached dispatch lag. Its 99th percentile rose to 150–180 µs at 200–205k, then fell to 1.5 µs at 215–230k, while latency's median was 70–300 ms. A dispatch lag limit could not catch the worst case.
2. **A shortfall was not a saturation reason.** At 230k, one run sent 827,953 of the 920,000 requests planned in its window in that window; the record showed only the count. Runs whose median latency stayed under 0.1 ms sent within 14 requests of their plan, or sent up to 482 more, catching up on the warmup's; runs whose median reached 5–400 ms fell 2,042 to 109,190 short.
3. **Two `recvfrom` calls per response.** Reading an edge-triggered socket until `EAGAIN` made 120,001 `recvfrom` calls for 60,000 responses, 60,000 of them failing with `EAGAIN`, where the fixed-rate client made 60,000. `recvfrom` was 14.6% of the client's syscall time.
4. **The busy limit was lenient.** At 140k, busy at 0.86–0.88 was not flagged by the 0.9 limit, though p99 was 1.3 times, and p99.9 2 to 3 times, the reference's. At 120k, busy at 0.76–0.77 coincided with quantiles within 1.1 times. A limit of 0.8 separates them. The healthy runs' 99th percentile dispatch lag, which equals the schedule lag when connections have room, as nearly all do at those loads, was at most 24 µs.
5. **CPU time leaves out softirq.** The client's CPU spent 14–23% of its time in software interrupts (from 100k to 230k), which the worker's thread CPU time leaves out. The server's `/proc/PID/stat` time missed 16–24% of its CPU time in the closed loop, the softirq on its CPUs: 8.16 against 9.92 µs per request at one connection, 3.70 against 4.89 at 64. The server side belongs to `base-benchmark`'s roles, which will measure the server.

## Not measured

The CPU model, hardware counters, the Rust toolchain version, and runs with more than one client worker; and any host other than hana.
