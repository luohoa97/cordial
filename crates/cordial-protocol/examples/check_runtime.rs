//! Run the launcher-side conformance cases against a runtime that is already
//! listening: `cargo run -p cordial-protocol --features conformance --example
//! check_runtime -- <session_dir> [--stop] [--id <runtime id>]`.
//!
//! `<session_dir>` is the directory holding the runtime's `ctl.sock`. The cases
//! connect more than once (a reattach, a second controller that supersedes the
//! first), so whatever launcher was attached to that runtime is replaced and
//! told so; point this at a runtime nothing else is controlling. `--stop` also
//! sends `lifecycle.stop`, which ends the runtime, so it runs last.
//!
//! Exit status 0 when no case failed. A skipped case is not a failure and is
//! printed as one.

use cordial_protocol::conformance::{run_against_runtime, Link, RuntimeOpts};
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: check_runtime <session_dir> [--stop] [--id <runtime id>]");
        return ExitCode::from(2);
    };
    let mut opts = RuntimeOpts::default();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--stop" => opts.test_stop = true,
            "--id" => opts.manifest_id = args.next(),
            other => {
                eprintln!("check_runtime: unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }
    let dir = std::path::PathBuf::from(dir);
    // A real runtime answers in milliseconds; five seconds is "silent".
    let mut connect = || -> std::io::Result<Link> {
        Link::unix(cordial_protocol::socket::connect(&dir)?, Duration::from_secs(5))
    };
    let report = run_against_runtime(&mut connect, &opts);
    println!("{report}");
    if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
