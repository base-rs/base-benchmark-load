# `base-benchmark-load`

An HTTP load generator for Base benchmarks: closed-loop maximum throughput, and open-loop fixed-rate latency measured from each request's intended send time.

**Status:** the first milestone: HTTP/1.1 keep-alive with pipelining, closed and open loops, validation of every response, errors by kind, fairness and saturation reporting, and a `base-benchmark` record. Linux only. Window markers for `base-benchmark` roles, Poisson arrivals, rate sweeps, TLS, and HTTP/2 come later; see [the architecture](context/architecture.md).

## Motivation

Base compares its servers and schedulers with Tokio and Hyper. The client that drives those comparisons must be a fixed, neutral instrument: it must not share code with the subjects, must not hide server stalls through coordinated omission, and must report its own errors, invalid responses, and saturation next to its measurements. Its results are `base-benchmark` process records, so it can act as the driver of `base-benchmark` campaigns.

## Usage

```sh
base-benchmark-load rate --address 127.0.0.1:8080 --rate 240k --connections 64 --cpus 2-5 --warmup 2s --duration 8s > record.json
base-benchmark-load closed --address 127.0.0.1:8080 --connections 64 --depth 4 --cpus 2-5 --duration 8s > record.json
```

The record goes to stdout and a summary to stderr. See [Using `base-benchmark-load`](context/usage.md) for the modes, what each distribution measures, how outcomes are counted, saturation, and the record, and the [requirements](design/load-generator.md) it is built to.

## Development

Install the [Bake launcher](https://github.com/base-rs/bake), then run `cargo bake test`, or `cargo bake test:coverage --all-targets true` for the coverage gate. Integration tests run the tool against a small in-test server and need Linux. The private `bake/` workspace member supplies shared Base development tasks and is not a dependency of the library.

## Releasing

Prepare a release with `cargo bake cargo:release:patch` (or `minor`, `major`, or `bump --version X.Y.Z`) from a clean checkout of `main`; it bumps the version, commits with the `Release: vVERSION` trailer, validates the candidate, and opens the pull request. After review and merge, GitHub Actions publishes the release when the configured `crates-io` environment approves it, then creates or updates the matching GitHub Release from `releases.md`. Follow the shared [Releasing skill](https://github.com/base-rs/base-project/blob/main/context/releasing.md) for the standard process.

## Releases

See [releases.md](releases.md).

## See Also

- [`base-benchmark`](https://github.com/base-rs/base-benchmark): the records this tool writes, and the runner that collects them.

## Contributing

Please open an issue or pull request on [GitHub](https://github.com/base-rs/base-benchmark-load).

### Agent Context

Run `cargo bake agent:context:install` to install shared context and skills. Use `cargo bake agent:context:list` and `cargo bake agent:context:show` to discover relevant guides, follow `.agents/implementation.md`, and apply skills under `.agents/skills/`.
