// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! An HTTP/1.1 load generator for Base benchmarks.
//!
//! It measures a server in one of two modes:
//!
//! - **closed**: every connection keeps a fixed number of requests outstanding, sending the next as soon as one completes. This measures maximum throughput, and reports the time from send to response as *service time* under saturation, not as latency.
//! - **rate**: requests follow an absolute schedule at a fixed offered rate, whatever the server does, and latency is measured from each request's *intended* send time. A server stall therefore stays visible in every request it delays, rather than being hidden by the client waiting (coordinated omission).
//!
//! Each worker is one thread, optionally pinned to a CPU, that busy-polls its own connections through epoll, with nothing shared while it runs. Every response is framed and validated: responses that fail validation are counted as invalid, and failed requests as errors by kind, next to the successes. Workers' histograms merge exactly. The result is one [`base_benchmark::ProcessRecord`] with a sample over the measured window, so the tool can be a `base-benchmark` workload, or the driver role of a multi-process scenario, which marks its window for the runner with [`run_with_window`].
//!
//! ```no_run
//! use base_benchmark_load::{Command, Options, run};
//!
//! let arguments = ["rate", "--address", "127.0.0.1:8080", "--rate", "10k"].map(String::from);
//! if let Command::Run(options) = Options::parse(arguments)? {
//!     let report = run(&options)?;
//!     eprint!("{}", report.summary());
//!     report.record()?.emit(&mut std::io::stdout())?;
//! }
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! See [the usage guide](https://github.com/base-rs/base-benchmark-load/blob/main/context/usage.md) for every option and metric.

mod clock;
mod connection;
mod histogram;
mod options;
mod os;
mod poller;
mod report;
mod request;
mod response;
mod run;
mod schedule;
mod statistics;
mod worker;

pub use histogram::Histogram;
pub use options::{BodyExpectation, Command, FailOn, Mode, Options, USAGE};
pub use report::{EXTENSION, Report, Spread, Starvation, WORKLOAD};
pub use run::{run, run_with_window};
pub use statistics::{
    Distributions, ErrorKind, IntervalStatistics, InvalidKind, Outcomes, Period, WorkerStatistics,
};

use std::{io::Write, process::ExitCode};

/// The usage guide's examples, compiled as doctests.
#[cfg(doctest)]
#[doc = include_str!("../context/usage.md")]
struct UsageGuide;

/// Run the command line `arguments`, without the program name: write the record to `out`, and the summary and errors to `errors`.
///
/// Under `base-benchmark`'s runner, which sets `BASE_BENCHMARK_CONTROL=stdio`, a run marks its measured window with [`base_benchmark::Window`], whose markers go to the process's stdout, before the record, and whose acknowledgement comes from its stdin, as [`run_with_window`] describes; `out` must then be stdout, as it is for the `base-benchmark-load` binary. Outside the runner, nothing is marked and stdin is never read.
///
/// Exits unsuccessfully, without a record, when the options are invalid, `BASE_BENCHMARK_CONTROL` names a control this version does not support, the run cannot start, its window cannot be marked, a `--fail-on` class occurred, or there is nothing to report.
pub fn command(
    arguments: impl IntoIterator<Item = String>,
    out: &mut impl Write,
    errors: &mut impl Write,
) -> ExitCode {
    let result = Options::parse(arguments).and_then(|command| match command {
        Command::Help => out.write_all(USAGE.as_bytes()),
        Command::Version => writeln!(out, "base-benchmark-load {}", env!("CARGO_PKG_VERSION")),
        Command::Run(options) => {
            let mut window = base_benchmark::Window::from_environment()?;
            let report = run_with_window(&options, window.as_mut())?;
            if !options.quiet {
                let _ = errors.write_all(report.summary().as_bytes());
            }
            let failures = report.failures(options.fail_on);
            if !failures.is_empty() {
                return Err(std::io::Error::other(failures.join("; ")));
            }
            report.record()?.emit(out)
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(errors, "base-benchmark-load: {error}");
            ExitCode::FAILURE
        }
    }
}
