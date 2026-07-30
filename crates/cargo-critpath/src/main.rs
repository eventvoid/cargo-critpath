//! The `cargo critpath` command.
//!
//! Everything it does is in the library beside it, so that the tests can drive the whole tool with
//! its streams captured. This file exists to hand it the process's arguments and its exit code.

#![forbid(unsafe_code)]

use std::io::Write;

fn main() {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let outcome = cargo_critpath::cli::run(std::env::args_os(), &mut out, &mut err);
    // A closed pipe is not something the caller can act on, and the exit code still has to be
    // right, so a failure to flush is not turned into a different outcome.
    let _ = out.flush();
    let _ = err.flush();
    std::process::exit(outcome.code());
}
