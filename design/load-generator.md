# A Neutral HTTP Load Generator

## Requirement

`base-benchmark-load` is a fixed instrument for measuring HTTP servers, used the same way against Base, Tokio, and Hyper subjects.

**Independence.** The client must not depend on `base-scheduler`, `base-http`, or any other subject it measures, so that a scheduler or parser bug cannot move the client and the server together. It parses response heads with `httparse` and frames bodies with its own code. Its I/O runs on plain epoll behind the thin `Poller` trait, so that another backend, such as io_uring, can be added without changing the workers. Dependencies stay minimal: `libc`, `httparse`, `serde_json`, and `base-benchmark` without its default runner feature; no async runtime and no scripting.

**Workers.** Each worker is one thread, pinned to its own CPU when CPUs are given, that owns its connections, its share of the schedule, and its statistics. Nothing is shared between workers on the hot path, and all workers start at one shared instant after every connection is established. Workers busy-poll with a zero timeout.

**Modes.**

- *Closed loop*: every connection keeps a fixed number of requests outstanding and sends the next as soon as one completes. Its send-to-response time is reported as service time under saturation, never as latency.
- *Open loop*: requests follow an absolute schedule at the offered rate, with request `k` due `floor(k × 10^9 / rate)` nanoseconds after the start. Offsets are computed from ordinals, never accumulated and never reset to an actual send or completion, so the schedule cannot drift. Latency is measured from each request's planned send time, so that a server stall stays visible in every request it delays (no coordinated omission). The cohort is every request planned in the measured window; it is drained after the window and never truncated to improve percentiles.
- Optional pipelining, up to a declared depth, in both modes. HTTP/1.1 keep-alive.

Run lengths, connection counts, and other limits are named options, not fixed constants. Limits that remain, such as the number of reporting intervals, must be stated with their reason.

**Measurement.**

- Histograms are log-linear with HdrHistogram's bucketing, per worker and per distribution, and merge exactly.
- Every response is validated: status, framing, and the expected body (by default the body of a preflight response), and optionally required headers or the exact bytes of the whole response.
- Responses that fail validation are counted as invalid, by reason, separately from successes and from errors, and are excluded from the latency distributions.
- Failed requests are counted by kind (write, read, reset, early close, timeout, parse, a closing response ahead of them, and never sent), next to the successes; the run continues and reconnects. Connection failures are counted as attempts.
- Every request of the cohort has exactly one outcome: valid, invalid, or failed.
- Per-connection counters give fairness across connections, and per-worker counters across workers.
- Client saturation is reported per worker as busy time and as dispatch lag, since busy-polling uses all of every worker CPU whatever the load.

**Output.** One `base-benchmark` process record on stdout, with one sample over the measured window, so the tool can act as the driver role of `base-benchmark` campaigns. Parameters contain everything that changes the work and nothing that varies between equal runs, such as the address or CPU placement. The record's signature identifies valid responses by status and body digest, or by the digest of the whole response, never by headers, whose case and order legitimately differ between servers.

**Tests.** No test may assert a wall-clock upper bound. Timing behaviour is tested with explicit times or a controlled clock, or by causality, such as a latency at least as long as a stall the test server imposed.

## Rationale

Samuel approved this design on 8 November 2026, from the load generator proposal and the `base-benchmark` roles proposal, in which this program is the driver role. It generalizes the hello benchmark's fixed-rate client, whose absolute schedule, pinned busy-polling epoll workers, drained cohort with exact counts, separate latency distributions, and byte-exact validation are kept. wrk is unmaintained, closed-loop only, and shares one histogram between threads; wrk2 has about 1 ms pacing accuracy, too coarse for the differences these benchmarks measure. A client built on a subject's own scheduler or HTTP parser would confound every comparison between Base and its comparators.

## Scope

The library, the `base-benchmark-load` command, its record, and its tests.

## Validation

- Inspect `cargo tree -e normal` for subject crates and an async runtime.
- Unit tests check the schedule's absence of drift and its inverse rounding, exact histogram merging, framing and validation of split, pipelined, chunked, close-delimited, and bodiless responses, error classification, and request timelines with explicit times.
- Integration tests against an in-test server check exact cohort counts across workers, that a 40 ms stall remains in open-loop latency with and without pipelining, that corrupted, mis-statused, and mis-headered responses are invalid, that each error kind is counted, and that setup failures fail the run without a record.
- Review tests for wall-clock upper bounds.
- Measurements of the tool itself, such as closed-loop throughput against wrk, belong in investigations, judged on an idle host, not in tests.
