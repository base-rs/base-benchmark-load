# Using `base-benchmark-load`

`base-benchmark-load` drives an HTTP/1.1 server and writes one `base-benchmark` process record. Use it to measure a server's maximum throughput, or its latency at a fixed offered rate. The `base-project-benchmarks` and `base-project-performance` skills remain the methodology authority: where benchmarks live, how to control the host, and how to judge results.

## Run it

```sh
cargo install base-benchmark-load
base-benchmark-load rate --address 127.0.0.1:8080 --rate 240k --connections 64 --cpus 2-5 --controller-cpu 1 --warmup 2s --duration 8s > record.json
base-benchmark-load closed --address 127.0.0.1:8080 --connections 64 --cpus 2-5 --duration 8s > record.json
```

The record goes to stdout as one line of JSON, and a summary to stderr. `base-benchmark-load help` lists every option. Linux is required.

Before any load, one **preflight** request on its own connection checks that the server answers with a valid response; its body becomes the body every response must repeat, unless `--body`, `--body-file`, `--any-body`, or `--response-file` says otherwise. Then every worker connects its share of the connections, round-robin, and all start together. A failure up to this point (an address that does not resolve, a refused or invalid preflight, a CPU that cannot be pinned, a connection that cannot be established within `--connect-timeout`) fails the run without a record.

## Modes

**`closed`** keeps `--depth` requests outstanding on every connection, sending the next as soon as one completes, for the warmup and then the window. At the end of the window, connections send nothing more and complete what they have in flight; those completions are the *drain*. The operations are the valid responses completed in the window, and the distribution is **service time**, from send to response. Under saturation this is a property of the queue the client built, not a latency the server offers, so the record never calls it latency.

**`rate`** sends at `--rate` requests per second on an absolute schedule. With `N` connections, connection `g` sends requests `g, g + N, g + 2N, …`, so each connection has its own evenly spaced schedule, and request `k` is due `floor(k × 10^9 / rate)` nanoseconds after the start. A connection with `--depth` requests outstanding keeps later requests in its own backlog and sends them as responses arrive; the schedule never moves. The **cohort** is every request planned in the window, however late it completes: after the window, the run drains until every planned request has an outcome, or until `--drain-timeout`. The operations are the planned cohort.

Rate mode reports four distributions of the cohort's valid responses:

| Distribution | From | To |
| --- | --- | --- |
| `latency` | planned send | complete response |
| `round_trip` | actual send | complete response |
| `send_delay` | planned send | actual send |
| `dispatch_lag` | the later of the planned send and the moment the connection had room | actual send |

`latency` includes the time a request waited behind a slow one, so a server stall stays visible in every request it delayed; `round_trip` does not. Percentiles of the components do not add up to percentiles of `latency`. `dispatch_lag` measures the client's own delay; closed mode reports it too.

## Outcomes

Every request has exactly one outcome:

- **valid**: a complete, correctly framed response that passes validation;
- **invalid**, by the first check it fails: `status`, a required `header`, the `body`, or the whole `response` (`--response-file`);
- **failed**, by kind: `write`, `read`, `reset`, `eof` (the server closed before the response was complete), `timeout` (no response within `--timeout` of the send, or still outstanding at the drain deadline), `parse` (an unframeable response, or bytes nobody asked for), `closed` (an earlier response said `Connection: close`), or `unsent` (planned, never sent before the drain deadline).

A failure closes the connection and charges every request outstanding on it; the connection reconnects after `--reconnect-delay` and the run continues. Failed connection attempts are counted separately, as attempts. Invalid responses are complete responses, so their connections stay open, but they are never in the distributions. `base-benchmark` should treat a record with invalid responses as making a comparison invalid; `--fail-on invalid,errors,saturated` exits unsuccessfully, without a record, instead.

Outcomes belong to **periods**: in rate mode by the planned time, so the measured cohort is exact; in closed mode by when the outcome happened, so warmup, window, and drain are separate.

## Saturation

Busy-polling uses every worker CPU fully whatever the load, so CPU time cannot show whether the client limited the load. Each worker reports the time it spent in loop iterations that did work, against idle polls, and its dispatch lag. The client is flagged as saturated when a worker was busy for more than `--busy-limit` (0.9) of the window, or when the 99th percentile dispatch lag exceeds `--dispatch-lag-limit`. A saturated open-loop run did not offer the rate it claims.

## The record

The workload is `base-benchmark-load/http1`. Parameters hold everything that changes the work: the mode, rate, connections, depth, workers, durations and timeouts, the request's method, path, length and digest, and the validation. The address and CPU placement are in the extension instead, since they vary between equal runs. The signature identifies valid responses, as `http1 status 200 body 13:DIGEST`, `http1 status 200 body any`, or `http1 response 78:DIGEST`. Digests are 64-bit FNV-1a.

The sample covers the window. Its metrics are:

- the controller's `Interval`: `wall_time`, process `cpu_time`, `user_cpu_time` and `system_cpu_time`, and context switches. The controller starts and stops it when it wakes at the window's boundaries, which the workers time exactly;
- counts of the measured period: `responses_valid`, `responses_invalid`, `invalid_KIND`, `errors`, `errors_KIND`, and `connect_errors` (over the whole run);
- `completed_in_window` and `sent_in_window`, by completion and send time;
- for each distribution, quantiles `NAME_p50`, `_p90`, `_p99`, `_p999`, and `_max`, and `NAME_mean`, in nanoseconds; unavailable without a valid response;
- `drain_time`, from the end of the window to the last outcome;
- `client_saturated` (0 or 1), `worker_busy_time_max`, and `connection_completions_min` and `_max`.

The `base-benchmark-load` extension holds the context (version, clock source, address, CPUs), the outcomes of every period, connection counts, bytes, fairness (minimum, median, maximum, mean, coefficient of variation, Jain's index, and zeros, of completions per connection, mean latency per connection, and completions per worker), each worker's busy fraction, CPU time, and dispatch lag, per-interval completions and latency quantiles (`--interval`), the full histograms as `[lowest value, count]` buckets with their precision, and with `--connection-details`, every connection's counters.

Histograms have three significant digits, with HdrHistogram's bucketing: a quantile is the highest value of the bucket holding it, clamped to the exact minimum and maximum. Merging per-worker histograms is exact.

## With `base-benchmark`

Until `base-benchmark` has roles, run the server yourself and use the record directly, or wrap both in a project-owned workload. Once roles exist, `base-benchmark-load` is the driver role: the service's ready line gives `{server.address}`, the role's CPU set gives `--cpus`, and the record becomes the sample. Window markers, which tell the runner when the measured window starts and ends, are not implemented yet; the controller marks the place in [the architecture](architecture.md).
