# Architecture

## Modules

| Module | Responsibility |
| --- | --- |
| `options` | The command line: named options with durations (`250ms`, `1.5s`) and rates (`240k`), validated together before anything runs. |
| `run` | The controller: resolves the address, builds the request, runs the preflight, raises the descriptor limit, spawns one worker per CPU, starts them together, measures the window with `base_benchmark::Interval`, and assembles the report. |
| `worker` | One thread's event loop: pins itself, connects its connections, busy-polls, sends on the schedule or in a closed loop, times out and reconnects, and stops when every request has an outcome. |
| `connection` | One connection's state machine, generic over its stream so tests can script it: queueing and writing requests, reading and completing responses, and failing every outstanding request with the connection's failure. |
| `response` | Incremental response framing and validation: heads through `httparse`, bodies by length, chunks, or close, compared with the expected body or whole response as they arrive. |
| `schedule` | The absolute open-loop schedule and each worker's cursor over its ordinals. |
| `statistics` | Outcomes by period and kind, the distributions, reporting intervals, and per-connection counters, with exact merging. |
| `histogram` | The log-linear histogram. |
| `report` | Merging, fairness, saturation, the record, and the summary. |
| `poller` and `os` | The `Poller` trait and its epoll implementation, and the few Linux calls the standard library lacks: CPU affinity, thread CPU time, the descriptor limit, and non-blocking connects. |
| `clock` | Monotonic nanoseconds from an origin, read only through the `Clock` trait. |

## The worker loop

Each iteration reads the clock once, accounts the previous iteration as busy if it did work, polls with a zero timeout, and handles events: a connection that finished connecting starts sending; a readable connection reads until it would block, completing every response in its buffer, then sends what it may and writes. In rate mode, the worker's cursor walks its ordinals in increasing order (`w + mW + jN` for worker `w` of `W` and connection `m` of `N`), so only the next due time is checked per iteration; when it passes, that ordinal's connection sends every due request it has room for. Every millisecond, maintenance times out requests and connection attempts, reconnects failed connections after their delay, and decides whether the worker is done.

Completion times are read after the read that completed them, and send times when the request is queued. A request's eligible time is the later of its planned time and the last time its connection went from full to having room, so dispatch lag is the client's delay alone.

Sockets are registered edge-triggered for reading, writing, and closing, once. A socket is removed from epoll before it is closed: closing alone leaves the registration in place while another process holds a duplicate of the descriptor, as a child does between `fork` and `exec`, and its events would then reach a connection that has moved on.

## Timing and periods

The controller publishes a start 100 ms after every worker has connected, so all see it in time. The warmup and window follow from it: `[start, start + warmup)` and `[start + warmup, start + warmup + duration)`. The drain deadline is `--drain-timeout` after the window. Rate-mode outcomes belong to the period of their planned time, closed-mode outcomes to the period they happen in.

## Window markers

`base-benchmark` roles will let the driver tell the runner when the window starts and ends, with a `start` marker acknowledged before the first counted request and a `stop` marker at the end. The controller is where they fit: it already wakes at both boundaries to start and stop its `Interval`. Since the rate-mode schedule is absolute, the `start` marker will have to be sent ahead of the window's start and acknowledged before it, or the window's start published after the acknowledgement; the closed loop can simply start counting after it.

## Not yet implemented

- Window markers and record version 3 histogram measurements, which wait for `base-benchmark`.
- `pool` assignment of arrivals to any idle connection, Poisson arrivals, rate sweeps, and `hybrid` pacing that sleeps until shortly before the next due request.
- Request templates beyond one fixed request, connection churn, TLS, HTTP/2, the echo protocol, io_uring, and platforms other than Linux.
- A differential framing test against `base-http1`.
