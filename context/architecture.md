# Architecture

## Modules

| Module | Responsibility |
| --- | --- |
| `options` | The command line: named options with durations (`250ms`, `1.5s`) and rates (`240k`), validated together before anything runs. |
| `run` | The controller: resolves the address, builds the request, runs the preflight, raises the descriptor limit, spawns one worker per CPU, pins itself once they exist, so that unpinned workers do not inherit its CPU, starts them together, measures the window with `base_benchmark::Interval`, marks it for `base-benchmark`'s runner with `base_benchmark::Window`, and assembles the report. |
| `worker` | One thread's event loop: pins itself, connects its connections, busy-polls, sends on the schedule or in a closed loop, times out and reconnects, and stops when every request has an outcome. |
| `connection` | One connection's state machine, generic over its stream so tests can script it: queueing and writing requests, reading and completing responses, and failing every outstanding request with the connection's failure. |
| `response` | Incremental response framing and validation: heads through `httparse`, bodies by length, chunks, or close, compared with the expected body or whole response as they arrive. |
| `schedule` | The absolute open-loop schedule and each worker's cursor over its ordinals. |
| `statistics` | Outcomes by period and kind, the distributions, reporting intervals, and per-connection counters, including each connection's longest wait for a response, with exact merging. |
| `histogram` | The log-linear histogram. |
| `report` | Merging, fairness, starvation, saturation, the record, and the summary. |
| `poller` and `os` | The `Poller` trait and its epoll implementation, and the few Linux calls the standard library lacks: CPU affinity, thread CPU time, the descriptor limit, non-blocking connects, and each CPU's software interrupt time from `/proc/stat`. |
| `clock` | Monotonic nanoseconds from an origin, read only through the `Clock` trait. |

## The worker loop

Each iteration reads the clock once, accounts the previous iteration as busy if it did work, polls with a zero timeout, and handles events: a connection that finished connecting starts sending; a readable connection reads what the socket holds, completing every response in its buffer, then sends what it may and writes. In rate mode, the worker's cursor walks its ordinals in increasing order (`w + mW + jN` for worker `w` of `W` and connection `m` of `N`), so only the next due time is checked per iteration; when it passes, the worker records how late it reached the ordinal, its schedule lag, and that ordinal's connection sends every due request it has room for. Every millisecond, maintenance times out requests and connection attempts, reconnects failed connections after their delay, and decides whether the worker is done.

Sockets are edge-triggered, so a connection must read until nothing is left, but it does not read again after a read that returned less than the buffer offered, which would only report that the socket would block: one `recvfrom` per response rather than two. A short TCP read has taken everything queued, including what arrived while it held the socket, except at a TCP urgent mark, where it stops short, and whatever arrives later wakes the socket again, which gives an edge-triggered registration a new event. Urgent data is not supported: the bytes after the mark would wait for an event that never comes, and their request ends in `--timeout`. The exception is an event that reports the peer's close or an error: the close may have arrived with the data before the event was reported, and no later event would report it, so the connection reads on until the end of the stream, the error, or a read that would block.

Completion times are read after the read that completed them, and send times when the request is queued. A request's eligible time is the later of its planned time and the last time the worker found its connection going from full to having room, so dispatch lag is a delay of the client's, but not all of it: the time a response waited to be read is left out. Schedule lag, from the planned time to the moment the cursor reaches the ordinal, includes it, as the length of the iterations that kept the worker from the cursor.

Sockets are registered edge-triggered for reading, writing, and closing, once. A socket is removed from epoll before it is closed: closing alone leaves the registration in place while another process holds a duplicate of the descriptor, as a child does between `fork` and `exec`, and its events would then reach a connection that has moved on.

## Timing and periods

The controller and the workers meet at a barrier twice: once every worker has pinned itself and connected, and again once the controller has published the start, or abandoned the run because a worker or the controller failed to set up. When a worker's thread cannot be spawned, the workers already started are waiting at the first meeting for the others, so the controller lowers the meeting's parties to them and itself, and abandons the run. The start is 100 ms after the first meeting, so every worker is waiting for it when it arrives. The warmup and window follow from it: `[start, start + warmup)` and `[start + warmup, start + warmup + duration)`. The drain deadline is `--drain-timeout` after the window. Rate-mode outcomes belong to the period of their planned time, closed-mode outcomes to the period they happen in.

## Starvation

Each connection's counters keep its requests planned in the window, set when the worker starts, and the longest wait of its requests outstanding in the window, with whether that request ended without a response. The wait is recorded where every request gets its outcome: when its response completes or it fails, from its send in the closed loop and its planned time in the open loop, which is how latency is measured there; and at the drain deadline, where a connection abandons its requests never sent, from the planned time of the oldest of them. A request whose outcome came before the window was not outstanding in it, and is left out.

The counters also follow the slot's starved span: when the open one began, the longest, and the longest part of one in the window. A `Connection` outlives its sockets, so the span goes on across failures and reconnections. It opens when a request is queued on a slot with none open, at the request's send in the closed loop or its planned time in the open loop; every response, valid or invalid, closes it and opens another at once if the slot still has a request in flight or due; a failure closes it if the slot has nothing due, which in the closed loop is after the window; and when the worker finishes, or abandons the slot at the drain deadline, from the planned time of its oldest request never sent if none was open, it closes the span that is left, in the closed loop at the window's end if the slot had nothing in flight. A span is measured from the window's start. The report judges each connection from its counters alone: starved when the server answered none of its requests of the measured period, which in the open loop needs requests planned in the window, or when its longest starved span is longer than `--starved-limit` of the window, and starved only in the drain when it was answered and only the drain made a span that long. Nothing about the workers or the shortfall enters the judgement, so starvation is never the client's saturation; the shortfall's `held_back` reason only states the starved connections beside its own attribution.

## Window markers

Under `base-benchmark`'s runner, `command` takes a `base_benchmark::Window` from the environment and `run_with_window` hands it to the controller, which alone writes the markers, on the process's stdout, and reads the acknowledgement from its stdin; the workers never touch either. Nothing else writes stdout before the record, which `command` writes only once `run_with_window` has returned, after the stop marker.

The start marker must be acknowledged before the window starts, so before every request planned in it, and every outcome the closed loop's window counts, but the schedule is absolute from the start, which the controller publishes at the second meeting, so the window cannot wait for a late acknowledgement. The controller marks the start in one of two places:

- when `--window-lead` is at least the warmup, right after the first meeting, when every worker has connected and waits for the start, and the services are idle. It publishes the start only once the acknowledgement has come, `START_LEAD` later, so every request of the run follows the snapshot causally. A failure here aborts the start, as a worker's failure to set up does, so no request is sent.
- otherwise, after publishing the start, when it wakes `--window-lead` before the window's start. It then compares the time it read the acknowledgement with the window's start, and a later one fails the run. The workers are already running and have no way to stop early, so a failure after the start is published, including a marker that cannot be written or an acknowledgement that cannot be read, is returned only once every worker has finished and been joined; no stop is marked after one.

In the closed loop, which counts the outcomes in the window, the stop is marked when the controller wakes at the window's end, before its `Interval` finishes. In rate mode, which counts the requests planned in the window, it is marked only once every worker has been joined, so every request has an outcome: the services' work for a request planned in the window but sent or served after it is then in their window, and since nothing is planned after the window, the drain holds no other requests' work but the warmup's stragglers. The report keeps when each marker was sent and acknowledged, on the run's clock, for the record's `window` extension and the summary.

## Not yet implemented

- Record version 3 histogram measurements, which wait for `base-benchmark`.
- `pool` assignment of arrivals to any idle connection, Poisson arrivals, rate sweeps, and `hybrid` pacing that sleeps until shortly before the next due request.
- Request templates beyond one fixed request, connection churn, TLS, HTTP/2, the echo protocol, io_uring, and platforms other than Linux.
- A differential framing test against `base-http1`.
