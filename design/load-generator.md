# A Neutral HTTP Load Generator

## Requirement

`base-benchmark-load` is a fixed instrument for measuring HTTP servers, used the same way against Base, Tokio, and Hyper subjects.

**Independence.** The client must not depend on `base-scheduler`, `base-http`, or any other subject it measures, so that a scheduler or parser bug cannot move the client and the server together. It parses response heads with `httparse` and frames bodies with its own code. Its I/O runs on plain epoll behind the thin `Poller` trait, so that another backend, such as io_uring, can be added without changing the workers. Dependencies stay minimal: `libc`, `httparse`, `serde_json`, and `base-benchmark` without its default runner feature; no async runtime and no scripting.

**Workers.** Each worker is one thread, pinned to its own CPU when CPUs are given, that owns its connections, its share of the schedule, and its statistics. Nothing is shared between workers on the hot path, and all workers start at one shared instant after every connection is established. Workers busy-poll with a zero timeout. The controlling thread may be pinned to a CPU of its own, never one of the workers' CPUs, and pinning it must not change where any worker may run.

**Modes.**

- *Closed loop*: every connection keeps a fixed number of requests outstanding and sends the next as soon as one completes. Its send-to-response time is reported as service time under saturation, never as latency.
- *Open loop*: requests follow an absolute schedule at the offered rate, with request `k` due `floor(k × 10^9 / rate)` nanoseconds after the start. Offsets are computed from ordinals, never accumulated and never reset to an actual send or completion, so the schedule cannot drift. Latency is measured from each request's planned send time, so that a server stall stays visible in every request it delays (no coordinated omission). The cohort is every request planned in the measured window; it is drained after the window and never truncated to improve percentiles.
- Optional pipelining, up to a declared depth, in both modes. HTTP/1.1 keep-alive.

Run lengths, connection counts, and other limits are named options, not fixed constants. Limits that remain, such as the number of reporting intervals and the response framing limits, must be stated with their reason.

**Measurement.**

- Histograms are log-linear with HdrHistogram's bucketing, per worker and per distribution, and merge exactly.
- Every response is validated: status, framing, and the expected body (by default the body of a preflight response), and optionally required headers or the exact bytes of the whole response. Framing follows RFC 9112 section 6.3, and a response it says ought to be handled as an error, such as one with both `Transfer-Encoding` and `Content-Length`, cannot be framed.
- Responses that fail validation are counted as invalid, by reason, separately from successes and from errors, and are excluded from the latency distributions.
- Failed requests are counted by kind (write, read, reset, early close, timeout, parse, a closing response ahead of them, and never sent), next to the successes; the run continues and reconnects. Connection failures are counted as attempts.
- Every request of the cohort has exactly one outcome: valid, invalid, or failed.
- The sample's operations are the measured period's valid responses only, in both modes, as `base-benchmark` requires of a driver, so that throughput and every per-operation metric count successes. Planned, invalid, and failed requests are reported apart. A run without a valid response in the measured period has no operations, and fails without a record.
- Per-connection counters give fairness across connections, and per-worker counters across workers.
- Client saturation is reported per worker as busy time and as schedule lag, the delay from each planned send to the moment the worker reached it whether or not its connection had room, since busy-polling uses all of every worker CPU whatever the load. An open-loop run is also saturated when the requests planned in its window were not sent in it, since the window then did not carry the offered rate. Default limits are justified by measurements on a measuring host, recorded in the repository.

**Output.** One `base-benchmark` process record on stdout, with one sample over the measured window, so the tool can act as the driver role of `base-benchmark` campaigns. Parameters contain everything that changes the work and nothing that varies between equal runs, such as the address, the default `Host` header that names it, or CPU placement. Metric directions rank servers only by properties of the server: its timings (latency, round trip, send delay, and service time), wall time per success, invalid responses, errors, and failed connection attempts are lower-is-better. The client's own diagnostics, such as its CPU time and context switches, dispatch and schedule lag, busy time, the shortfall of sends in the window, and saturation, are neutral. The record's signature identifies valid responses by status and body digest, or by the digest of the whole response, never by headers, whose case and order legitimately differ between servers.

**Tests.** No test may assert a wall-clock upper bound, or depend on how much happens in a short time. Timing behaviour is tested with explicit times or a controlled clock, or by causality, such as a latency at least as long as a stall the test server imposed. Runs end on deterministic conditions, such as every planned request having an outcome, with timeouts long enough to be only watchdogs unless the test is about them. Coverage must not depend on timing: every region is reached by a deterministic test.

## Rationale

Samuel approved this design on 8 November 2026, from the load generator proposal and the `base-benchmark` roles proposal, in which this program is the driver role. Counting only successes as operations follows the roles requirement that the driver's operation count is its successful operations only ([`base-benchmark` roles, requirement 7](https://github.com/base-rs/base-benchmark/blob/main/design/roles.md)); before it, the open loop counted every planned request, which made time per operation the inverse of the offered rate. The address is left out of parameters because, under roles, it comes from each service's ready line and so differs between every sample. It generalizes the hello benchmark's fixed-rate client, whose absolute schedule, pinned busy-polling epoll workers, drained cohort with exact counts, separate latency distributions, and byte-exact validation are kept. wrk is unmaintained, closed-loop only, and shares one histogram between threads; wrk2 has about 1 ms pacing accuracy, too coarse for the differences these benchmarks measure. A client built on a subject's own scheduler or HTTP parser would confound every comparison between Base and its comparators.

Saturation was first judged by busy time and dispatch lag. An evaluation on the measuring host hana on 9 November 2026 showed that dispatch lag falls as a worker falls behind, since a response read late counts as its connection having had no room, so it could not catch the worst overload, and that a run could fall 10% short of sending its window's planned requests without being flagged; schedule lag and the shortfall replaced it, as the maintainer authorised as bug fixes. The evaluation is recorded in `results/2026-11-09-hana`.

## Scope

The library, the `base-benchmark-load` command, its record, and its tests.

## Validation

- Inspect `cargo tree -e normal` for subject crates and an async runtime.
- Unit tests check the schedule's absence of drift and its inverse rounding, exact histogram merging, framing and validation of split, pipelined, chunked, close-delimited, and bodiless responses, error classification, and request timelines with explicit times.
- Integration tests against an in-test server check exact cohort counts across workers, that a 40 ms stall remains in open-loop latency with and without pipelining, that corrupted, mis-statused, and mis-headered responses are invalid, that each error kind is counted, that runs without a valid response and setup failures fail without a record, that parameters are equal for servers on different ports, and, from each thread's affinity, that a pinned controller leaves unpinned workers where they were.
- Review tests for wall-clock upper bounds and for counts that depend on how much happens in a short time. Run the whole suite in parallel repeatedly on a loaded host, such as alongside `stress --cpu` with one worker per CPU, and expect no failures.
- Compare coverage run in parallel, single-threaded, and on a loaded host: the uncovered regions must be the same.
- Measurements of the tool itself, such as closed-loop throughput against wrk, belong in investigations, judged on an idle host, not in tests.
