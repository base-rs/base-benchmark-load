# Architecture

## Modules

| Module | Responsibility |
| --- | --- |
| `options` | The command line: named options with durations (`250ms`, `1.5s`) and rates (`240k`), validated together before anything runs. |
| `run` | The controller: resolves the address, builds the request, runs the preflight, raises the descriptor limit, spawns one worker per CPU, pins itself once they exist, so that unpinned workers do not inherit its CPU, starts them together, measures the window with `base_benchmark::Interval`, and assembles the report. |
| `worker` | One thread's event loop: pins itself, connects its connections, busy-polls, sends on the schedule or in a closed loop, times out and reconnects, and stops when every request has an outcome. |
| `connection` | One connection's state machine, generic over its stream so tests can script it: queueing and writing requests, reading and completing responses, and failing every outstanding request with the connection's failure. |
| `response` | Incremental response framing and validation: heads through `httparse`, bodies by length, chunks, or close, compared with the expected body or whole response as they arrive. |
| `schedule` | The absolute open-loop schedule and each worker's cursor over its ordinals. |
| `statistics` | Outcomes by period and kind, the distributions, reporting intervals, and per-connection counters, with exact merging. |
| `histogram` | The log-linear histogram. |
| `report` | Merging, fairness, saturation, the record, and the summary. |
| `poller` and `os` | The `Poller` trait and its epoll implementation, and the few Linux calls the standard library lacks: CPU affinity, thread CPU time, the descriptor limit, non-blocking connects, and each CPU's software interrupt time from `/proc/stat`. |
| `clock` | Monotonic nanoseconds from an origin, read only through the `Clock` trait. |

## The worker loop

Each iteration reads the clock once, accounts the previous iteration as busy if it did work, polls with a zero timeout, and handles events: a connection that finished connecting starts sending; a readable connection reads what the socket holds, completing every response in its buffer, then sends what it may and writes. In rate mode, the worker's cursor walks its ordinals in increasing order (`w + mW + jN` for worker `w` of `W` and connection `m` of `N`), so only the next due time is checked per iteration; when it passes, the worker records how late it reached the ordinal, its schedule lag, and that ordinal's connection sends every due request it has room for. Every millisecond, maintenance times out requests and connection attempts, reconnects failed connections after their delay, and decides whether the worker is done.

Sockets are edge-triggered, so a connection must read until nothing is left, but it does not read again after a read that returned less than the buffer offered, which would only report that the socket would block: one `recvfrom` per response rather than two. A short TCP read has taken everything queued, including what arrived while it held the socket, and whatever arrives later wakes the socket again, which gives an edge-triggered registration a new event. The exception is an event that reports the peer's close or an error: the close may have arrived with the data before the event was reported, and no later event would report it, so the connection reads on until the end of the stream, the error, or a read that would block.

Completion times are read after the read that completed them, and send times when the request is queued. A request's eligible time is the later of its planned time and the last time the worker found its connection going from full to having room, so dispatch lag is a delay of the client's, but not all of it: the time a response waited to be read is left out. Schedule lag, from the planned time to the moment the cursor reaches the ordinal, includes it, as the length of the iterations that kept the worker from the cursor.

Sockets are registered edge-triggered for reading, writing, and closing, once. A socket is removed from epoll before it is closed: closing alone leaves the registration in place while another process holds a duplicate of the descriptor, as a child does between `fork` and `exec`, and its events would then reach a connection that has moved on.

## Timing and periods

The controller and the workers meet at a barrier twice: once every worker has pinned itself and connected, and again once the controller has published the start, or abandoned the run because a worker or the controller failed to set up. When a worker's thread cannot be spawned, the workers already started are waiting at the first meeting for the others, so the controller lowers the meeting's parties to them and itself, and abandons the run. The start is 100 ms after the first meeting, so every worker is waiting for it when it arrives. The warmup and window follow from it: `[start, start + warmup)` and `[start + warmup, start + warmup + duration)`. The drain deadline is `--drain-timeout` after the window. Rate-mode outcomes belong to the period of their planned time, closed-mode outcomes to the period they happen in.

## Window markers

`base-benchmark` roles will let the driver tell the runner when the window starts and ends, with a `start` marker acknowledged before the first counted request and a `stop` marker at the end. The controller is where they fit: it already wakes at both boundaries to start and stop its `Interval`. Since the rate-mode schedule is absolute, the `start` marker will have to be sent ahead of the window's start and acknowledged before it, or the window's start published after the acknowledgement; the closed loop can simply start counting after it.

## Not yet implemented

- Window markers and record version 3 histogram measurements, which wait for `base-benchmark`.
- `pool` assignment of arrivals to any idle connection, Poisson arrivals, rate sweeps, and `hybrid` pacing that sleeps until shortly before the next due request.
- Request templates beyond one fixed request, connection churn, TLS, HTTP/2, the echo protocol, io_uring, and platforms other than Linux.
- A differential framing test against `base-http1`.
