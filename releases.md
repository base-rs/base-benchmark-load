# Releases

## Unreleased

- Establish the repository and shared Base development tooling.
- Add the HTTP/1.1 load generator, generalized from the hello benchmark's fixed-rate client: a closed loop for maximum throughput, reported as service time, and an open loop on an absolute schedule with latency from each request's planned send time, both with optional pipelining. Workers are pinned, busy-polling epoll threads that share nothing while running.
- Validate every response: framing by length, chunks, or close, the status, the expected body (by default the preflight response's), required headers, or the exact whole response. Count invalid responses by reason and failed requests by kind next to the successes, and reconnect after failures; every planned request has exactly one outcome.
- Report HdrHistogram-style distributions merged exactly across workers, per-interval statistics, fairness across connections and workers, and client saturation by busy time and dispatch lag, in one `base-benchmark` process record.
