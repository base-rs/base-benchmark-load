// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

fn parse(arguments: &[&str]) -> io::Result<Command> {
    Options::parse(arguments.iter().map(|argument| argument.to_string()))
}

fn options(arguments: &[&str]) -> Options {
    match parse(arguments).unwrap() {
        Command::Run(options) => *options,
        command => panic!("expected options, not {command:?}"),
    }
}

fn error(arguments: &[&str]) -> String {
    let error = parse(arguments).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    error.to_string()
}

#[test]
fn defaults() {
    let options = options(&["closed", "--address", "127.0.0.1:80"]);
    assert_eq!(
        options,
        Options {
            address: "127.0.0.1:80".to_owned(),
            ..Options::new(Mode::Closed)
        }
    );
    assert_eq!(options.workers, 1);
    assert_eq!(Mode::Closed.name(), "closed");
    assert_eq!(Mode::Rate.name(), "rate");
}

#[test]
fn every_option() {
    let path = std::env::temp_dir().join(format!(
        "base-benchmark-load-options-{}",
        std::process::id()
    ));
    std::fs::write(&path, b"file body").unwrap();
    let file = path.to_str().unwrap();
    let options = options(&[
        "rate",
        "--address=localhost:8080",
        "--connections",
        "8",
        "--depth",
        "2",
        "--rate",
        "1.5k",
        "--cpus",
        "2,3,6-7",
        "--workers",
        "4",
        "--controller-cpu",
        "1",
        "--warmup",
        "0",
        "--duration",
        "1.5s",
        "--timeout",
        "250ms",
        "--connect-timeout",
        "2s",
        "--drain-timeout",
        "1m",
        "--reconnect-delay",
        "100us",
        "--interval",
        "500ms",
        "--method",
        "POST",
        "--path",
        "/echo?x=1",
        "--header",
        "Accept:  */* ",
        "--header",
        "X-Tab: a\tb",
        "--request-body",
        "hello",
        "--status",
        "201",
        "--body-file",
        file,
        "--expect-header",
        "Content-Type: text/plain",
        "--connection-details",
        "--busy-limit",
        "0.5",
        "--dispatch-lag-limit",
        "50µs",
        "--fail-on",
        "invalid,errors",
        "--fail-on",
        "saturated",
        "--quiet",
    ]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(options.address, "localhost:8080");
    assert_eq!(
        (options.connections, options.depth, options.workers),
        (8, 2, 4)
    );
    assert_eq!(options.rate, NonZeroU64::new(1500));
    assert_eq!(options.cpus, [2, 3, 6, 7]);
    assert_eq!(options.controller_cpu, Some(1));
    assert_eq!(options.warmup, Duration::ZERO);
    assert_eq!(options.duration, Duration::from_millis(1500));
    assert_eq!(options.timeout, Duration::from_millis(250));
    assert_eq!(options.connect_timeout, Duration::from_secs(2));
    assert_eq!(options.drain_timeout, Duration::from_secs(60));
    assert_eq!(options.reconnect_delay, Duration::from_micros(100));
    assert_eq!(options.interval, Duration::from_millis(500));
    assert_eq!(
        (options.method.as_str(), options.path.as_str()),
        ("POST", "/echo?x=1")
    );
    assert_eq!(
        options.headers,
        [
            ("Accept".to_owned(), "*/*".to_owned()),
            ("X-Tab".to_owned(), "a\tb".to_owned())
        ]
    );
    assert_eq!(options.request_body, b"hello");
    assert_eq!(options.status, 201);
    assert_eq!(options.body, BodyExpectation::Exact(b"file body".to_vec()));
    assert_eq!(
        options.expect_headers,
        [("Content-Type".to_owned(), "text/plain".to_owned())]
    );
    assert!(options.connection_details && options.quiet);
    assert_eq!(options.busy_limit, 0.5);
    assert_eq!(options.dispatch_lag_limit, Some(Duration::from_micros(50)));
    assert_eq!(
        options.fail_on,
        FailOn {
            invalid: true,
            errors: true,
            saturated: true
        }
    );
}

#[test]
fn bodies_and_responses() {
    let base = ["closed", "--address", "a:1"];
    let with = |extra: &[&str]| options(&[&base[..], extra].concat());
    assert_eq!(
        with(&["--body", "x"]).body,
        BodyExpectation::Exact(b"x".to_vec())
    );
    assert_eq!(with(&["--any-body"]).body, BodyExpectation::Any);
    let path = std::env::temp_dir().join(format!(
        "base-benchmark-load-response-{}",
        std::process::id()
    ));
    std::fs::write(&path, b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
    let response = with(&["--response-file", path.to_str().unwrap()]).response;
    std::fs::remove_file(&path).unwrap();
    assert_eq!(response.as_deref(), Some(&b"HTTP/1.1 200 OK\r\n\r\n"[..]));
    let missing =
        parse(&[&base[..], &["--response-file", "/nonexistent/response"]].concat()).unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert!(
        missing
            .to_string()
            .contains("reading /nonexistent/response")
    );
}

#[test]
fn help_and_version() {
    for help in [&["help"][..], &["--help"], &["-h"], &["closed", "--help"]] {
        assert_eq!(parse(help).unwrap(), Command::Help);
    }
    assert_eq!(parse(&["version"]).unwrap(), Command::Version);
    assert_eq!(parse(&["--version"]).unwrap(), Command::Version);
    assert!(USAGE.contains("--fail-on"));
}

#[test]
fn workers_follow_the_cpus() {
    let base = ["closed", "--address", "a:1"];
    assert_eq!(
        options(&[&base[..], &["--cpus", "1-3"]].concat()).workers,
        3
    );
    assert_eq!(
        options(&[&base[..], &["--workers", "3"]].concat()).workers,
        3
    );
    assert_eq!(
        options(&[&base[..], &["--workers", "3", "--cpus", "1-3"]].concat()).workers,
        3
    );
    assert!(
        error(&[&base[..], &["--workers", "2", "--cpus", "1-3"]].concat())
            .contains("does not match")
    );
}

#[test]
fn invalid_command_lines() {
    for (arguments, message) in [
        (&[][..], "a mode is required"),
        (&["open"], "unknown mode"),
        (&["closed"], "--address is required"),
        (&["closed", "--address"], "--address needs a value"),
        (&["closed", "--bogus"], "unknown option"),
        (&["closed", "--quiet=yes"], "takes no value"),
        (&["closed", "--connections", "0"], "positive integer"),
        (&["closed", "--connections", "x"], "positive integer"),
        (&["closed", "--controller-cpu", "1024"], "CPU number"),
        (&["closed", "--cpus", "3-1"], "increasing CPU range"),
        (&["closed", "--cpus", "1,1"], "names a CPU twice"),
        (&["closed", "--cpus", ""], "CPU number"),
        (&["closed", "--cpus", "1-x"], "CPU number"),
        (&["closed", "--warmup", "1h"], "not a duration"),
        (&["closed", "--warmup", "."], "not a duration"),
        (&["closed", "--warmup", "1.0000000001s"], "not a duration"),
        (&["closed", "--warmup", "0.5ns"], "not a duration"),
        (
            &["closed", "--warmup", "99999999999999999999s"],
            "not a duration",
        ),
        (&["closed", "--warmup", "1e3"], "not a duration"),
        (
            &[
                "closed",
                "--warmup",
                "1234567890123456789012345678901234567890",
            ],
            "not a duration",
        ),
        (
            &["closed", "--warmup", "123456789012345678901234567890123456"],
            "not a duration",
        ),
        (&["rate", "--rate", "0"], "positive whole number"),
        (&["rate", "--rate", "1.5"], "positive whole number"),
        (&["rate", "--rate", "x"], "positive whole number"),
        (&["closed", "--busy-limit", "2"], "fraction"),
        (&["closed", "--status", "100"], "final status"),
        (&["closed", "--fail-on", "slow"], "unknown --fail-on class"),
        (&["closed", "--method", "G T"], "HTTP token"),
        (&["closed", "--method", ""], "HTTP token"),
        (&["closed", "--path", "/\r\n"], "control characters"),
        (&["closed", "--path", ""], "control characters"),
        (&["closed", "--header", "no colon"], "not a header"),
        (&["closed", "--header", "Bad Name: x"], "HTTP token"),
        (&["closed", "--header", "X: a\rb"], "control characters"),
    ] {
        assert!(
            error(arguments).contains(message),
            "{arguments:?}: {} lacks {message}",
            error(arguments)
        );
    }
}

#[test]
fn invalid_combinations() {
    for (arguments, message) in [
        (&["rate", "--address", "a:1"][..], "needs --rate"),
        (
            &["closed", "--address", "a:1", "--rate", "1"],
            "only to rate mode",
        ),
        (
            &[
                "closed",
                "--address",
                "a:1",
                "--connections",
                "2",
                "--workers",
                "3",
            ],
            "cannot be shared",
        ),
        (
            &["closed", "--address", "a:1", "--duration", "0"],
            "must be positive",
        ),
        (
            &["closed", "--address", "a:1", "--interval", "0"],
            "must be positive",
        ),
        (
            &["closed", "--address", "a:1", "--timeout", "0"],
            "must be positive",
        ),
        (
            &["closed", "--address", "a:1", "--connect-timeout", "0"],
            "must be positive",
        ),
        (
            &[
                "closed",
                "--address",
                "a:1",
                "--duration",
                "1s",
                "--interval",
                "1us",
            ],
            "lengthen --interval",
        ),
        (
            &[
                "rate",
                "--address",
                "a:1",
                "--rate",
                "1",
                "--warmup",
                "0.5",
                "--duration",
                "0.4",
            ],
            "no request is planned",
        ),
        (
            &[
                "closed",
                "--address",
                "a:1",
                "--any-body",
                "--response-file",
                "/dev/null",
            ],
            "replaces",
        ),
        (
            &[
                "closed",
                "--address",
                "a:1",
                "--expect-header",
                "A: b",
                "--response-file",
                "/dev/null",
            ],
            "replaces",
        ),
    ] {
        assert!(
            error(arguments).contains(message),
            "{arguments:?}: {}",
            error(arguments)
        );
    }
    // A window of exactly the interval limit is accepted.
    options(&[
        "closed",
        "--address",
        "a:1",
        "--duration",
        "100ms",
        "--interval",
        "1us",
    ]);
}

#[test]
fn durations_and_rates() {
    for (text, nanoseconds) in [
        ("2", 2_000_000_000),
        ("1.5", 1_500_000_000),
        (".5ms", 500_000),
        ("7ns", 7),
        ("3us", 3000),
        ("2m", 120_000_000_000),
    ] {
        assert_eq!(
            duration(text).unwrap(),
            Duration::from_nanos(nanoseconds),
            "{text}"
        );
    }
    for (text, value) in [
        ("240000", 240_000),
        ("240k", 240_000),
        ("240K", 240_000),
        ("1.5m", 1_500_000),
        ("2M", 2_000_000),
    ] {
        assert_eq!(rate(text).unwrap().get(), value, "{text}");
    }
    assert_eq!(nanoseconds(Duration::MAX), u64::MAX);
    assert_eq!(nanoseconds(Duration::from_nanos(5)), 5);
}
