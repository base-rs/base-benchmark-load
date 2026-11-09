# Using `base-benchmark-load`

`base-benchmark-load` drives an HTTP/1.1 server and writes one `base-benchmark` process record. Use it to measure a server's maximum throughput, or its latency at a fixed offered rate. The `base-project-benchmarks` and `base-project-performance` skills remain the methodology authority: where benchmarks live, how to control the host, and how to judge results.

## Run it

```sh
cargo install base-benchmark-load --locked --version VERSION
base-benchmark-load rate --address 127.0.0.1:8080 --rate 240k --connections 64 --cpus 2-5 --controller-cpu 1 --warmup 2s --duration 8s > record.json
base-benchmark-load closed --address 127.0.0.1:8080 --connections 64 --cpus 2-5 --duration 8s > record.json
```

The record goes to stdout as one line of JSON, and a summary to stderr. `base-benchmark-load help` lists every option. Linux is required. Pin the version you install, so that every run of a comparison uses the same instrument; the record's context names the version that ran.

`--cpus` pins one worker to each CPU it lists. `--controller-cpu` pins the thread that controls the run, which mostly sleeps, to a CPU of its own: it may not be one of `--cpus`. The controller is pinned only after the workers have started, so workers without a CPU of their own keep the affinity the process had, rather than inheriting the controller's. The record lists each worker's CPU and the CPUs it could run on.

Before any load, one **preflight** request on its own connection checks that the server answers with a valid response; its body becomes the body every response must repeat, unless `--body`, `--body-file`, `--any-body`, or `--response-file` says otherwise. Then every worker connects its share of the connections, round-robin, and all start together. A failure up to this point (an address that does not resolve, a refused or invalid preflight, a worker thread that cannot be started for lack of threads or memory, a CPU that cannot be pinned, a connection that cannot be established within `--connect-timeout`) fails the run without a record.

## Modes

**`closed`** keeps `--depth` requests outstanding on every connection, sending the next as soon as one completes, for the warmup and then the window. At the end of the window, connections send nothing more and complete what they have in flight; those completions are the *drain*, which lasts until every request in flight has an outcome, or until `--drain-timeout`. The requests in flight at the window's end are never abandoned at it: the summary states their outcomes on a line of their own, and how long they waited counts towards [starvation](#starved-connections). The operations are the valid responses completed in the window, and the distribution is **service time**, from send to response. Under saturation this is a property of the queue the client built, not a latency the server offers, so the record never calls it latency.

**`rate`** sends at `--rate` requests per second on an absolute schedule. With `N` connections, connection `g` sends requests `g, g + N, g + 2N, …`, so each connection has its own evenly spaced schedule, and request `k` is due `floor(k × 10^9 / rate)` nanoseconds after the start. A connection with `--depth` requests outstanding keeps later requests in its own backlog and sends them as responses arrive; the schedule never moves. The **cohort** is every request planned in the window, however late it completes: after the window, the run drains until every planned request has an outcome, or until `--drain-timeout`. The operations are the cohort's valid responses; the planned, invalid, and failed requests are counted apart.

In both modes, operations are successful requests only, as `base-benchmark` requires of a driver, so time per operation is the inverse of the throughput the server achieved, and errors per operation are errors per success. A run without a valid response in the measured period has no operations: it writes its summary, then fails without a record.

Rate mode reports four distributions of the cohort's valid responses, and one of every request planned in the window:

| Distribution | From | To |
| --- | --- | --- |
| `latency` | planned send | complete response |
| `round_trip` | actual send | complete response |
| `send_delay` | planned send | actual send |
| `dispatch_lag` | the later of the planned send and the moment the worker found the connection had room | actual send |
| `schedule_lag` | planned send | the moment the worker reached the request on its schedule, whether or not its connection had room |

`latency` includes the time a request waited behind a slow one, so a server stall stays visible in every request it delayed; `round_trip` does not. Percentiles of the components do not add up to percentiles of `latency`.

The two lags are the client's own delays. `schedule_lag` is how late the worker was in getting to each due request: the length of the loop iterations it was busy with, or the time its thread could not run, as when another process preempted it. It does not wait for the server's responses, and it rises steadily with the worker's load. `dispatch_lag` is how long the worker took to send a request once it knew it could: a connection gains room when the worker reads the response that frees it, so a response that waited to be read, because the worker was busy with others, counts as the connection having had no room. It is the schedule lag of requests whose connection had room when they fell due, and nearly zero for the others, so as a worker falls behind and more connections are full, it falls instead of rising: on one overloaded core, its 99th percentile fell from 180 µs to 1.5 µs while latency rose to hundreds of milliseconds. Only `schedule_lag` judges saturation. Closed mode, which has no schedule, reports `dispatch_lag` alone, and there it is nearly zero, since a connection sends as soon as the worker reads the response that freed it.

## Outcomes

Every request has exactly one outcome:

- **valid**: a complete, correctly framed response that passes validation;
- **invalid**, by the first check it fails: `status`, a required `header`, the `body`, or the whole `response` (`--response-file`);
- **failed**, by kind: `write`, `read`, `reset`, `eof` (the server closed before the response was complete), `timeout` (no response within `--timeout` of the send, or still outstanding at the drain deadline), `parse` (an unframeable response, or bytes nobody asked for), `closed` (an earlier response closed the connection: it said `Connection: close`, or its body was delimited by the close), or `unsent` (planned, never sent before the drain deadline).

A failure closes the connection and charges every request outstanding on it; the connection reconnects after `--reconnect-delay` and the run continues. Failed connection attempts are counted separately, as attempts. Invalid responses are complete responses, so their connections stay open, but they are never in the distributions. A `base-benchmark` scenario that declares `invalid-responses = "responses_invalid"` treats a record with invalid responses as making its comparisons invalid; `--fail-on invalid,errors,saturated,starved` exits unsuccessfully, without a record, instead.

Outcomes belong to **periods**: in rate mode by the planned time, so the measured cohort is exact; in closed mode by when the outcome happened, so warmup, window, and drain are separate.

## Starved connections

A server can accept a connection and never serve it, or serve it for a while and then stop. Puma, with more keep-alive connections than threads, serves a connection only when a thread frees up, which with keep-alive never happens, so the connections it accepted last wait unread for the whole run. The distributions hold only the valid responses of the measured period, so such a run can show good throughput and a healthy latency or service time; a client that abandons the requests outstanding at the end of the run, as wrk does, hides the starved connections entirely. So every run judges each connection, and a connection is **starved** when:

- the server answered none of its requests of the measured period, with a valid or an invalid response: in the closed loop, none completed in the window; in the open loop, none of its requests planned in the window, if it had any; or
- it had a **starved span** longer than `--starved-limit` of the window (0.25, a quarter of it).

A starved span is a time during which the connection had a request outstanding or due to send, and the server sent it no response, valid or invalid. It follows the connection's slot, not one TCP connection: when a request times out after `--timeout`, or the connection fails, and it reconnects, the span goes on, as long as the slot has a request to send, which in the closed loop it always has in the window, and in the open loop when a request's planned time has come. It begins at the slot's last response, or, if it had nothing outstanding or due then, when its next request became due: at its send in the closed loop, or its planned time in the open loop. It counts from the window's start, and one still open at the window's end runs on through the drain, until a response, until the slot has nothing outstanding or due, or until the drain deadline. So a connection the server stops serving partway through the window has a span from its last response to the end of the run, however often its requests time out, while a fair server, however slow, answers every connection at least once a round trip, so its spans stay near its round trip.

The limit is a fraction of the window rather than `--timeout`, whose default, 10 seconds, is longer than a typical window, so that starvation for part of the window would go unreported. A fair server reaches a quarter of the window only with a round trip that long, at most four responses a connection in the window. Lower `--starved-limit` to catch shorter starvation, as in a long window; raise it for a run whose round trip is meant to be long, such as a closed loop with far more connections than the server serves at once. In a window so short that the limit is shorter than the median round trip, even a fair server exceeds it, and the reason says so.

A connection whose starved spans are longer than the limit only with the drain, which the server answered in the window and left without a response for no longer than the limit in it, is **starved only in the drain**. A server that waits on a keep-alive connection for its next request does this after a closed loop's window, when the client stops sending: the requests other connections had in flight wait until they time out. It is the server's behaviour, so it is reported, but in the window those connections' spans were within the limit, so `--fail-on starved` ignores it. That does not make the window's measurement of them fair: a connection the server stopped answering just under the limit before the window's end, and then through a long drain, is starved only in the drain too.

The longest wait for a response is reported beside the spans: the longest any request outstanding in the window waited, from its send in the closed loop or its planned time in the open loop to its response, its failure, or, for a planned request never sent, the drain deadline. A request still unanswered at the end of the run has a wait that ends when it fails, at `--timeout` after its send or at the drain deadline, which is a lower bound of how long the server would have kept it waiting, and the summary says so. The summary states, right after the outcomes and before any distribution, how many of the connections were starved, by each criterion, how many only in the drain, the longest starved span, and the longest wait, as in:

```text
STARVED: 4 of 8 connections (50.00%) were starved: the server left 4 of them without a response, with a request outstanding or due, for longer than 1.500 s (25% of the 6.000 s window); the longest the server left a connection without a response was 5.011 s, and the longest wait for a response was at least 1.002 s, and its request was never answered
```

or `starved: none of 8 connections; the longest the server left a connection without a response was 1.20 ms, and the longest wait for a response was 1.20 ms`. `--fail-on starved` exits unsuccessfully, without a record, when any connection was starved in the window.

Starvation is the server's doing, and never makes the client saturated: a worker reads every response as it arrives, so it can leave a connection without a response for long only by not running, which its busy time and schedule lag show. When the client was saturated, the reason says that its own delays may have lengthened the spans and waits: a worker that falls behind reads responses late, and in rate mode reaches requests late, and a span or wait from a planned time includes that schedule lag. A connection that is reconnecting after failures, or that the server refuses, is starved too if the server answered none of its requests, or left it without a response for longer than the limit; its errors and failed connection attempts say why. In rate mode, a starved connection's later requests wait behind the unanswered one, as the [shortfall](#shortfall)'s connections' part, and its reason then also states how many connections were starved. A server that is merely overloaded is not starving its connections as long as it keeps answering each of them: its latency, not its spans, grows with the backlog.

## Framing

Responses are framed as RFC 9112 section 6.3 describes: no body for `HEAD` requests and `204` and `304` responses, chunked when chunked is the final transfer coding, delimited by the close for any other coding or without a length, and otherwise by `Content-Length`. A response is a `parse` failure when it has both `Transfer-Encoding` and `Content-Length`, which the RFC says ought to be handled as an error since the two may disagree; when its `Content-Length` is empty, not decimal, or disagrees with another; or when it reaches a limit:

| Limit | Value | Why |
| --- | ---: | --- |
| Response head | 64 KiB | Each connection buffers one incomplete head, so this bounds its memory, for thousands of connections. Servers send heads of hundreds of bytes. |
| Header fields in one head | 100 | `httparse` parses into a fixed array of this many slots; it is hyper's default. |
| Chunk-size or trailer line | 4,096 bytes | An incomplete line is buffered and searched for its end. A chunk size has at most 16 hexadecimal digits, which leaves room for extensions and trailers. |

Each is far beyond what a benchmarked server sends, so reaching one means the bytes are not the response they claim to be.

## Saturation

Busy-polling uses every worker CPU fully whatever the load, so CPU time cannot show whether the client limited the load. Each worker reports the time it spent in loop iterations that did work, against idle polls, and its schedule lag. The client is flagged as saturated, with each reason in the summary and the record, when:

- a worker was busy for more than `--busy-limit` (0.8) of the window;
- in rate mode, a worker's 99th percentile schedule lag exceeds `--schedule-lag-limit` (50 µs);
- in rate mode, the workers reached more than `--shortfall-limit` (0.001) of the requests planned in the window only after it: they fell further behind their schedule over the window, as the [shortfall](#shortfall)'s client part shows.

A saturated open-loop run did not offer the rate it claims. The defaults come from [an evaluation on a measuring host](../results/2026-11-09-hana/readme.md), with one worker against a raw server: at 120,000 requests per second the worker was busy for 76–77% of the window and latency quantiles were within 1.1 times the reference client's; at 140,000 it was busy for 86–88%, with a 99th percentile 1.3 times, and a 99.9th 2 to 3 times, the reference's. The healthy runs' 99th percentile dispatch lag, which equals the schedule lag when connections have room, as nearly all do at those loads, was at most 24 µs; the schedule lag limit is twice that. The busy limit is conservative on a faster host, where a worker can be busier before its latency suffers: on a Ryzen 9950X3D, a worker busy for 82–85% of the window at 500,000 requests per second kept its 99th percentile within the reference client's. Raise it for such a host with evidence from it, rather than for one result. The schedule lag limit is absolute, too: on a slower or noisier host, where interrupts and other processes delay the worker more, or in a short or low-rate run, whose few samples put the 99th percentile near the maximum, a client that kept up may exceed it. Raise it for such runs with evidence from them, in the same way.

### Shortfall

In rate mode, the requests planned in the window less those sent in it, `sent_in_window_shortfall`, is how much the backlog of requests due but unsent grew over the window, which stays within a few requests when the run keeps up. When it grows, the window did not carry the rate it claims. A worker reaches every request on its schedule before it sends it, whether or not the request's connection has room, so the record tells which side held the requests back:

- `schedule_shortfall`, the requests planned in the window less those the workers reached in it, is the client's part: the workers fell behind their schedule. Over `--shortfall-limit` of the requests planned in the window, it saturates the client. A worker reaches every due request in each iteration of its loop, so this part stays within an iteration's requests unless a worker stops running across the window's end; a worker that is merely overloaded shows as busy time and schedule lag instead, and the responses it reads late keep its connections full, which counts in the other part.
- `connection_shortfall`, the requests the workers reached in the window less those they sent in it, is the connections' part: the workers reached the requests, but the requests' connections were awaiting responses, with as many requests outstanding as `--depth` allows, or were reconnecting after a failure. This is not the client's saturation, so in an open-loop sweep past the server's capacity, `--fail-on saturated` keeps the records of runs whose client kept its schedule, and latency shows what the server's falling behind cost.

Every request is reached before it is sent, so the shortfall is the schedule's difference plus the connections'. Either difference is negative when the workers caught up, in the window, on a backlog from before it: workers that start the window behind their schedule reach more requests in it than were planned in it, and workers that send a backlog they reached before it send more than they reach. So each part is its difference clamped to between zero and the shortfall, and the two parts always add up to the shortfall: they are the differences when both are positive, and otherwise the positive one, up to the shortfall, is all of it. A window that carried its rate has no shortfall, and no parts, however far behind its schedule it began.

When the connections' part is over `--shortfall-limit` of the requests planned in the window, the summary and the record's `shortfall.held_back` give the reason; otherwise `held_back` is null. The reason names the server as holding the requests back only when the client is not saturated and no worker reached any request planned in the window later than `--schedule-lag-limit`. A worker that stalled, even briefly just before the window's end, too briefly for its busy time or 99th percentile schedule lag to show it, may have read late the responses that make room, so otherwise the reason names the connections, with how many requests they may have outstanding, as in "the connections (4 × 1 outstanding) held back …", and why the server is not named. Either way, the reason states the round trip under which `--connections` × `--depth` outstanding requests carry the offered rate: connections too few or too shallow for the server's healthy round trip are full whatever the server does, which is a limit of the run's configuration, not the server's overload, and needs more connections or depth. When the server starved connections, the reason ends by saying how many, as in "; 2 of the 4 connections were starved, with requests the server left waiting": their requests wait on the server however many connections there are and however deep.

A worker's CPU time (`cpu_ns`) is its thread's, and leaves out the time its CPU spends in software interrupts, which `/proc/stat` counts apart from every thread's time on kernels that account interrupt time separately, as the evaluation host's did: on loopback, much of it delivers the worker's own packets, about a fifth of a busy client CPU in the evaluation. Each pinned worker reports its CPU's software interrupt time over the controller's window, from `/proc/stat`, as `softirq_ns`. It counts in ticks of the kernel's `USER_HZ`, usually 10 ms, and includes whatever else interrupted that CPU. The controller's process `cpu_time` leaves it out too.

## The record

The workload is `base-benchmark-load/http1`. Parameters hold everything that changes the work, and nothing that varies between equal runs: the mode, rate, connections, depth, workers, durations and timeouts, the request, and the validation. The request is its method and path, `host` (`default` when the `Host` header names the address, as it does unless `--header` gives one, or `header`), and the length and digest of the request without a default `Host` header. The address, and so the default `Host` header, and CPU placement are in the extension instead: under `base-benchmark` roles the address comes from the server's ready line, and differs for every sample. The signature identifies valid responses, as `http1 status 200 body 13:DIGEST`, `http1 status 200 body any`, or `http1 response 78:DIGEST`. Digests are 64-bit FNV-1a.

The sample covers the window. Its metrics are:

- the controller's `Interval`: `wall_time`, process `cpu_time`, `user_cpu_time` and `system_cpu_time`, and context switches. The controller starts and stops it when it wakes at the window's boundaries, which the workers time exactly;
- counts of the measured period: `responses_valid`, `requests_planned` (rate mode), `responses_invalid`, `invalid_KIND`, `errors`, `errors_KIND`, and `connect_errors` (over the whole run);
- `completed_in_window` and `sent_in_window`, by completion and send time;
- `sent_in_window_shortfall` (rate mode): the requests planned in the window less those sent in it, or zero when more were sent, and its [parts](#shortfall), `schedule_shortfall` and `connection_shortfall`;
- for each distribution, quantiles `NAME_p50`, `_p90`, `_p99`, `_p999`, and `_max`, and `NAME_mean`, in nanoseconds;
- `drain_time`, from the end of the window to the last outcome;
- `client_saturated` (0 or 1), `worker_busy_time_max`, and `connection_completions_min` and `_max`;
- `connections_starved` and `connections_starved_fraction`, the [starved connections](#starved-connections), including those starved only in the drain, and their share of the connections, and, as maxima attached to the window, since they run on into the drain, `starved_span_max`, the longest starved span, and `response_wait_max`, the longest any request outstanding in the window waited for its response, which is a lower bound when that request was never answered, as the extension's `starvation.wait_max_unanswered` says.

`base-benchmark` divides the measured counts by the operations, so they read per valid response: `errors` is errors per success, and `requests_planned` the planned requests per success. Only properties of the server have a direction. Lower is better for `wall_time`, which per success is the inverse of the throughput; the `latency`, `round_trip`, `send_delay`, and `service_time` distributions, since a request waits to be sent mostly behind a slow response; `responses_invalid` and `invalid_KIND`; `errors` and `errors_KIND`; `connect_errors`; and `connections_starved`, `connections_starved_fraction`, `starved_span_max`, and `response_wait_max`, since a server that starves connections is worse whatever its throughput. Every other metric is a diagnostic of the client, and neutral: the controller's `cpu_time`, `user_cpu_time`, `system_cpu_time`, and context switches, which measure this process; the `dispatch_lag` and `schedule_lag` distributions; `responses_valid` and `requests_planned`; the counts in the window and the shortfalls; `drain_time`; `client_saturated`; `worker_busy_time_max`; and the connection completions. In the closed loop a faster server makes the client busier, so read saturation as a reason to distrust a result, not as a difference between servers.

The `base-benchmark-load` extension holds the context (version, clock source, address, CPUs, and the descriptor limit in effect), the [`window`](#the-measured-window) markers' timing under the runner, or null, `preflight_requests`, the requests sent before the load, which have no outcome, so a server's own request count is the sum of every period's outcomes and these; the outcomes of every period, connection counts, bytes, fairness (minimum, median, maximum, mean, coefficient of variation, Jain's index, and zeros, of completions per connection, mean latency per connection, and completions per worker), the saturation reasons and the limits that judged them, `starvation` (the connections, the starved ones, by each criterion as `unanswered` and `long_span`, those starved only in the drain as `drain_only`, their fraction, the `limit` and `span_limit_ns`, `span_max_ns`, `wait_max_ns`, `wait_max_unanswered`, and the reason, or null), the shortfall, its parts, and `held_back`, the reason the connections or the server held requests back, or null, each worker's CPU, the CPUs it could run on, busy fraction, CPU time, its CPU's software interrupt time, and 99th percentile dispatch and schedule lag, per-interval completions and latency quantiles (`--interval`), the full histograms as `[lowest value, count]` buckets with their precision, and with `--connection-details`, every connection's counters, including its requests planned in the window (rate mode), whether it was starved, and only in the drain, its longest starved span and that span's part in the window, and its longest wait.

Histograms have three significant digits, with HdrHistogram's bucketing: a quantile is the highest value of the bucket holding it, clamped to the exact minimum and maximum. Merging per-worker histograms is exact.

## With `base-benchmark`

`base-benchmark-load` is the driver role of `base-benchmark` multi-process scenarios, from `base-benchmark` 0.4: the server is a service whose ready line gives its address, and the client's record is the sample. Install it at a pinned version as an external program, and give its path to every run:

```sh
cargo install --locked --root tools base-benchmark-load@VERSION
base-benchmark run hello --artifacts target/base-benchmark/prepared-TIME/artifacts.json --program load=tools/bin/base-benchmark-load --role-cpus client=1,5 --role-cpus server=2,3
```

```toml
[[package.metadata.base-benchmark.scenario]]
name = "hello"

[[package.metadata.base-benchmark.scenario.role]]
name = "server"
kind = "service"
subject = true
ready = "listening on {address}"

[[package.metadata.base-benchmark.scenario.role]]
name = "client"
kind = "driver"
external = "load"
invalid-responses = "responses_invalid"
arguments = ["rate", "--address", "{server.address}", "--rate", "100k", "--connections", "64", "--cpus", "1", "--controller-cpu", "5"]
smoke-arguments = ["--warmup", "100ms", "--duration", "200ms"]
measure-arguments = ["--warmup", "2s", "--duration", "8s"]
```

- **Invalid responses.** Declare `invalid-responses = "responses_invalid"`: it is the record's total of the measured period's responses that failed validation, which `base-benchmark` requires of the driver, so that a screening campaign reports comparisons with any as invalid and a recorded one fails. Leave `invalid` out of `--fail-on` here, since without a record the sample fails instead. The operations are the valid responses only, and errors are ordinary metrics, per success.
- **Placement.** `--role-cpus client=...` confines the whole process; `--cpus` and `--controller-cpu` then pin the workers and the controller to CPUs within it, which the role's CPU set must include.
- **Parameters.** The address comes from the server's ready line, and is not a parameter, so every sample's record has the same parameters.

### The measured window

Under the runner, which sets `BASE_BENCHMARK_CONTROL=stdio` in the driver's environment, the controller marks the measured window with [`base_benchmark::Window`](https://docs.rs/base-benchmark/0.4.0/base_benchmark/struct.Window.html): it writes `base-benchmark window start` to stdout and waits for the runner's acknowledgement on stdin, which the runner sends once it has snapshotted every service's CPU time, then writes `base-benchmark window stop` when the window ends, and only then the record. The runner measures the services' CPU between the two, which summaries divide by the operations as CPU per request. Outside the runner the variable is unset, nothing is marked, stdin is never read, and the record is all that goes to stdout. A `BASE_BENCHMARK_CONTROL` this version does not support fails the run.

The start is acknowledged before the window starts, so the services' snapshot precedes every request the window counts in rate mode, all of them planned in it. In the closed loop the window counts the outcomes that happen in it, and the requests in flight across its start, at most `--depth` per connection, were sent during the warmup, so they may reach the server before the snapshot; that is part of the approximation at the window's edges, as the requests in flight at its end are. Every worker starts at one instant, 100 ms after all have connected, so:

- with `--window-lead` at least as long as `--warmup`, as with no warmup, the start is marked once every worker has connected, and the load's start is decided only once it is acknowledged, so no request of the run, warmup or window, precedes the services' snapshot. The services' window then also covers the warmup;
- otherwise, the load starts first, and the start is marked `--window-lead` (100 ms) before the window, during the warmup. The services' window then covers that much of the warmup's load too, about 1% of an 8-second window, at the window's rate. If the acknowledgement arrives after the window has started, as on a host so busy that the runner or the controller is delayed by longer than the lead, the run fails once its requests have outcomes, without a record, and says how late it was: the services' snapshot may then follow counted requests. Lengthen the lead for such a host, or shorten it on an idle measuring host to tighten the window's edge. [An evaluation](../results/2026-11-09-aiko-window/readme.md) on a busy desktop found acknowledgements of 37 µs to 3.3 ms, and a 10 ms lead failed one sample of two.

The stop is marked when the controller wakes at the window's end, with the controller's own interval; the runner snapshots the services then without delaying the client. The record's `window` extension says when the markers were sent: `lead_ns`, `before_start`, whether the start was marked before the load started, `acknowledgement_ns`, from the start marker to its acknowledgement, `acknowledged_before_ns`, how long before the window's start it came, and `stop_after_ns`, how long after the window's end the stop was marked. It is null outside the runner. The summary says the same on a line of its own.

Nothing else is written to stdout: `help` and `version` write lines that never begin `base-benchmark window`, and the record is one line of JSON. The summary and errors go to stderr.

## As a library

A project can run the tool from its own workspace instead of a separate install, with a wrapper binary that depends on `base-benchmark-load` at a pinned version and calls [`command`](https://docs.rs/base-benchmark-load/latest/base_benchmark_load/fn.command.html), which takes the command line without the program name and behaves exactly as the installed command does:

```rust no_run
fn main() -> std::process::ExitCode {
    base_benchmark_load::command(
        std::env::args().skip(1),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}
```

The `base-benchmark-load` binary is this wrapper, and under the runner marks the window on the process's stdout, so the wrapper must pass stdout as `out`. To use the measurements without a record, parse the options with `Options::parse`, call `run`, and read the `Report`: its summary, its statistics, and its record. `run_with_window` takes a `base_benchmark::Window` to mark the window with, as `command` does with `Window::from_environment()`.
