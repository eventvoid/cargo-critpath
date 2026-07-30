//! The command line itself: what the arguments mean, and which exit code each outcome produces.
//!
//! The exit codes are a contract with CI scripts, documented in `docs/json-schema.md`, so each of
//! the four is pinned by a test rather than left to whichever branch happens to be taken.

// A test asserting an outcome is allowed to unwrap: a panic is the failure report.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use common::{fixture, run, run_workspace, scratch_bytes, scratch_file};

/// A timing report in the target alphabet `cargo check` produces, recorded in
/// `docs/cargo-data-sources.md`: a library renders as `" (check)"` and a binary as
/// `" name \"bin\" (check)"`. Two units are enough to be refused by name.
const CHECK_REPORT: &str = r#"<html><body>
<table class="my-table summary-table">
<tr><td>Profile:</td><td>dev</td></tr>
<tr><td>Fresh units:</td><td>0</td></tr>
<tr><td>Dirty units:</td><td>2</td></tr>
<tr><td>Total units:</td><td>2</td></tr>
<tr><td>Max concurrency:</td><td>2 (jobs=4 ncpu=10)</td></tr>
<tr><td>Total time:</td><td>0.5s</td></tr>
</table>
<script>
const UNIT_DATA = [
  {"i":0,"name":"leaf","version":"0.1.0","mode":"todo","target":" (check)","features":[],
   "start":0.0,"duration":0.2,"unblocked_units":[],"unblocked_rmeta_units":[],"sections":null},
  {"i":1,"name":"app","version":"0.1.0","mode":"todo","target":" demo \"bin\" (check)","features":[],
   "start":0.2,"duration":0.3,"unblocked_units":[],"unblocked_rmeta_units":[],"sections":null}
];
</script>
</body></html>
"#;

#[test]
fn help_is_written_to_stdout_and_completes() {
    let run = run(&["--help"]);
    assert_eq!(run.code(), 0);
    assert!(run.err.is_empty(), "{}", run.err);
    for expected in [
        "--log",
        "--timings",
        "--metadata",
        "--jobs",
        "--format",
        "--policy",
        "--include-units",
        "--top",
        "--fail-if-critical-path-over",
        "--version",
    ] {
        assert!(run.out.contains(expected), "help omits {expected}");
    }
}

#[test]
fn version_is_written_to_stdout_and_completes() {
    let run = run(&["--version"]);
    assert_eq!(run.code(), 0);
    assert!(run.out.contains(env!("CARGO_PKG_VERSION")), "{}", run.out);
}

#[test]
fn the_subcommand_word_cargo_passes_is_skipped() {
    let bare = run_workspace(&["--format", "json"]);
    let mut args = vec!["critpath".to_owned()];
    args.extend(common::workspace());
    args.push("--format".to_owned());
    args.push("json".to_owned());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let prefixed = run(&borrowed);

    assert_eq!(prefixed.code(), 0, "{}", prefixed.err);
    assert_eq!(prefixed.out, bare.out);
}

#[test]
fn an_unknown_option_names_itself_and_lists_the_valid_ones() {
    let run = run_workspace(&["--jobz", "4"]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("--jobz"), "{}", run.err);
    assert!(run.err.contains("--jobs"), "{}", run.err);
    assert!(run.err.contains("--timings"), "{}", run.err);
    assert!(run.out.is_empty(), "{}", run.out);
}

#[test]
fn a_positional_argument_that_is_not_the_subcommand_is_a_usage_error() {
    let run = run_workspace(&["extra"]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("extra"), "{}", run.err);
}

#[test]
fn the_log_is_required() {
    let run = run(&["--timings", &fixture("workspace/cargo-timing.html")]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("--log"), "{}", run.err);
}

#[test]
fn at_least_one_timing_report_is_required() {
    let run = run(&["--log", &fixture("workspace/verbose.log")]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("--timings"), "{}", run.err);
}

#[test]
fn an_option_that_needs_a_value_and_has_none_is_a_usage_error() {
    let run = run(&["--log"]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("--log"), "{}", run.err);
}

#[test]
fn a_path_that_cannot_be_read_is_a_usage_error_naming_it() {
    let run = run(&[
        "--log",
        "no-such-build.log",
        "--timings",
        &fixture("workspace/cargo-timing.html"),
    ]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("no-such-build.log"), "{}", run.err);
}

#[test]
fn a_job_count_of_zero_is_a_usage_error() {
    let run = run_workspace(&["--jobs", "0"]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("--jobs"), "{}", run.err);
}

#[test]
fn an_unknown_format_is_a_usage_error_listing_the_formats() {
    let run = run_workspace(&["--format", "yaml"]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("yaml"), "{}", run.err);
    assert!(run.err.contains("human"), "{}", run.err);
    assert!(run.err.contains("json"), "{}", run.err);
}

#[test]
fn an_unknown_policy_is_a_usage_error_listing_the_policies() {
    let run = run_workspace(&["--policy", "shortest"]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("cargo-like"), "{}", run.err);
    assert!(run.err.contains("longest-path"), "{}", run.err);
}

#[test]
fn an_incremental_build_is_rejected_rather_than_analysed() {
    let run = run(&[
        "--log",
        &fixture("incremental/verbose.log"),
        "--timings",
        &fixture("incremental/cargo-timing.html"),
    ]);
    assert_eq!(run.code(), 2);
    assert!(run.err.contains("cargo clean"), "{}", run.err);
}

#[test]
fn a_truncated_timing_report_is_rejected() {
    let run = run(&[
        "--log",
        &fixture("workspace/verbose.log"),
        "--timings",
        &fixture("truncated/cargo-timing.html"),
    ]);
    assert_eq!(run.code(), 2);
    assert!(run.err.contains("UNIT_DATA"), "{}", run.err);
}

#[test]
fn a_report_from_cargo_check_is_refused_by_name() {
    let path = scratch_file("check-timing.html", CHECK_REPORT);
    let run = run(&[
        "--log",
        &fixture("workspace/verbose.log"),
        "--timings",
        &path,
    ]);
    assert_eq!(run.code(), 2);
    assert!(run.err.contains("cargo check"), "{}", run.err);
    assert!(run.err.contains("cargo build"), "{}", run.err);
}

#[test]
fn a_threshold_that_is_exceeded_exits_three_and_still_reports() {
    let run = run_workspace(&["--fail-if-critical-path-over", "1000"]);
    assert_eq!(run.code(), 3);
    assert!(run.out.contains("1310ms"), "{}", run.out);
    assert!(run.err.contains("1310"), "{}", run.err);
}

#[test]
fn a_threshold_that_is_met_completes() {
    let run = run_workspace(&["--fail-if-critical-path-over", "2000"]);
    assert_eq!(run.code(), 0, "{}", run.err);
}

#[test]
fn a_threshold_that_is_not_a_number_is_a_usage_error() {
    let run = run_workspace(&["--fail-if-critical-path-over", "soon"]);
    assert_eq!(run.code(), 1);
    assert!(run.err.contains("soon"), "{}", run.err);
}

#[test]
fn a_log_in_utf16_is_rejected_as_input_whether_or_not_it_carries_a_mark() {
    // PowerShell's redirection writes UTF-16. With a byte-order mark the file is not valid UTF-8 at
    // all; without one every second byte is a NUL, which is. Both are the same mistake and both
    // have to be reported as input this tool cannot use, not as a bad command line: a script that
    // branches on the exit code must not see the difference.
    let utf16: Vec<u8> = "   Compiling demo v0.1.0\n Running `/tc/bin/rustc --crate-name demo`\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut with_mark = vec![0xFF, 0xFE];
    with_mark.extend_from_slice(&utf16);

    for (name, bytes) in [("utf16-bom.log", &with_mark), ("utf16-plain.log", &utf16)] {
        let path = scratch_bytes(name, bytes);
        let run = run(&[
            "critpath",
            "--log",
            &path,
            "--timings",
            &fixture("workspace/cargo-timing.html"),
        ]);
        assert_eq!(run.code(), 2, "{name}: expected input to be rejected");
        assert!(
            run.err.contains("UTF-16"),
            "{name}: the message must name the encoding, got: {}",
            run.err
        );
    }
}
