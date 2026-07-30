//! What the timing report must yield, and what it must refuse.
//!
//! The report is an undocumented internal format, so these tests describe the shape observed on
//! cargo 1.95.0 and pin the refusals that keep a changed shape from turning into a wrong number.

// A test asserting a parse result is allowed to unwrap: a panic is the failure report.
// The integer division below is a deliberate `len * 3 / 4` cut point for a truncation test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic
)]

use cargo_critpath::{ParseError, TimingTarget};
use critpath_core::Millis;

fn report(fresh: usize, dirty: usize, units: &str) -> String {
    format!(
        "<html><head><title>Cargo Build Timings</title></head><body>\n\
         <table class=\"my-table summary-table\">\n\
         <tr>\n<td>Targets:</td><td>demo 0.1.0 ( demo \"bin\")</td>\n</tr>\n\
         <tr>\n<td>Profile:</td><td>dev</td>\n</tr>\n\
         <tr>\n<td>Fresh units:</td><td>{fresh}</td>\n</tr>\n\
         <tr>\n<td>Dirty units:</td><td>{dirty}</td>\n</tr>\n\
         <tr>\n<td>Total units:</td><td>{total}</td>\n</tr>\n\
         <tr>\n<td>Max concurrency:</td><td>12 (jobs=10 ncpu=8)</td>\n</tr>\n\
         <tr>\n<td>Total time:</td><td>61.2s (1m 1.2s)</td>\n</tr>\n\
         <tr>\n<td>rustc:</td><td>rustc 1.95.0 (59807616e 2026-04-14)<br>\
         Host: aarch64-apple-darwin<br>Target: x86_64-unknown-linux-gnu</td>\n</tr>\n\
         </table>\n\
         <script>\nDURATION = 62;\nconst UNIT_DATA = [\n{units}\n];\n\
         const CONCURRENCY_DATA = [\n{{\"t\":0.5,\"active\":2,\"waiting\":1,\"inactive\":0}}\n];\n\
         const CPU_USAGE = [\n[0.0,12.5],[0.5,90.0]\n];\n</script></body></html>\n",
        total = fresh + dirty,
    )
}

const LIB: &str = r#"{"i":3,"name":"serde_json","version":"1.0.145","mode":"todo","target":"",
  "features":["std","default"],"start":1.02,"duration":2.31,
  "unblocked_units":[],"unblocked_rmeta_units":[7],
  "sections":[["frontend",{"start":0.0,"end":1.83}],["codegen",{"start":1.83,"end":2.31}]]}"#;

const BIN: &str = r#"{"i":7,"name":"demo","version":"0.1.0","mode":"todo","target":" demo \"bin\"",
  "features":[],"start":3.33,"duration":0.4,
  "unblocked_units":[],"unblocked_rmeta_units":[],"sections":[]}"#;

const SCRIPT: &str = r#"{"i":1,"name":"libc","version":"0.2.186","mode":"todo",
  "target":" build-script","features":[],"start":0.1,"duration":0.2,
  "unblocked_units":[2],"unblocked_rmeta_units":[],"sections":[]}"#;

const SCRIPT_RUN: &str = r#"{"i":2,"name":"libc","version":"0.2.186","mode":"run-custom-build",
  "target":" build-script (run)","features":[],"start":0.3,"duration":0.05,
  "unblocked_units":[3],"unblocked_rmeta_units":[],"sections":[]}"#;

fn full() -> String {
    report(0, 4, &format!("{SCRIPT},\n{SCRIPT_RUN},\n{LIB},\n{BIN}"))
}

#[test]
fn durations_become_whole_milliseconds() {
    let parsed = cargo_critpath::parse_timing_report(&full()).unwrap();
    assert_eq!(parsed.units.len(), 4);
    let lib = &parsed.units[2];
    // 2.31 s and a frontend ending at 1.83 s: the report's resolution is 10 ms, so nothing is lost.
    assert_eq!(lib.duration, Millis::new(2_310));
    assert_eq!(lib.start, Millis::new(1_020));
    assert_eq!(lib.metadata_at, Some(Millis::new(1_830)));

    // A unit with no `frontend` section emitted no metadata anyone could wait on.
    assert_eq!(parsed.units[3].metadata_at, None);
}

#[test]
fn the_target_string_is_the_only_thing_that_says_what_a_unit_is() {
    let parsed = cargo_critpath::parse_timing_report(&full()).unwrap();
    let kinds: Vec<&TimingTarget> = parsed.units.iter().map(|u| &u.target).collect();
    assert!(matches!(kinds[0], TimingTarget::BuildScriptCompile));
    assert!(matches!(kinds[1], TimingTarget::BuildScriptRun));
    // A plain library and a procedural macro are indistinguishable here; both render as "".
    assert!(matches!(kinds[2], TimingTarget::Library));
    match kinds[3] {
        TimingTarget::Named { name, kind } => {
            assert_eq!(name, "demo");
            assert_eq!(kind, "bin");
        }
        other => panic!("expected a named target, got {other:?}"),
    }
}

#[test]
fn the_summary_table_supplies_the_build_context() {
    let parsed = cargo_critpath::parse_timing_report(&full()).unwrap();
    assert_eq!(parsed.fresh_units, 0);
    assert_eq!(parsed.dirty_units, 4);
    assert_eq!(parsed.total_units, 4);
    assert_eq!(parsed.jobs, Some(10));
    assert_eq!(parsed.cpus, Some(8));
    // Observed concurrency can exceed the job limit, because a build-script run is not a compiler
    // process and takes no compile token.
    assert_eq!(parsed.max_concurrency, Some(12));
    assert_eq!(parsed.profile.as_deref(), Some("dev"));
    assert_eq!(
        parsed.rustc_version.as_deref(),
        Some("rustc 1.95.0 (59807616e 2026-04-14)")
    );
    assert_eq!(parsed.host.as_deref(), Some("aarch64-apple-darwin"));
    assert_eq!(parsed.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
    // "61.2s (1m 1.2s)": the leading value is the one to read.
    assert_eq!(parsed.reported_total, Some(Millis::new(61_200)));
}

#[test]
fn the_compilation_span_excludes_whatever_happened_before_the_first_unit() {
    let parsed = cargo_critpath::parse_timing_report(&full()).unwrap();
    // First unit starts at 0.10 s; the last one ends at 3.33 + 0.40 = 3.73 s.
    assert_eq!(parsed.compilation_span(), Millis::new(3_630));
    // 61.2 s reported against 3.63 s of compilation: 57.57 s of fetching and resolution that no
    // graph transformation can shorten.
    assert_eq!(parsed.excluded_prefix(), Millis::new(57_570));
}

#[test]
fn the_unblocked_fields_are_kept_although_they_are_not_the_graph() {
    let parsed = cargo_critpath::parse_timing_report(&full()).unwrap();
    // They record only the last blocker of each unit, so they cover about half the real edges.
    // They are kept because every pair in them must still be an edge the log agreed on.
    assert_eq!(parsed.units[2].unblocked_rmeta, vec![7]);
    assert_eq!(parsed.units[0].unblocked, vec![2]);
    assert_eq!(parsed.units[2].index, 3);
}

#[test]
fn a_report_from_an_incremental_build_is_refused() {
    // Cargo 1.95 lists fresh units with a zero duration; older releases omit them. The header's
    // counts are the reliable signal either way, so the check reads them rather than the array.
    let text = report(27, 1, LIB);
    let err = cargo_critpath::parse_timing_report(&text).unwrap_err();
    assert_eq!(
        err,
        ParseError::FreshUnits {
            fresh: 27,
            total: 28
        }
    );
    let message = err.to_string();
    assert!(message.contains("cargo clean"), "{message}");
}

#[test]
fn a_report_with_no_unit_data_is_refused() {
    let text = "<html><body><table><tr><td>Fresh units:</td><td>0</td></tr></table></body></html>";
    assert_eq!(
        cargo_critpath::parse_timing_report(text).unwrap_err(),
        ParseError::NoUnitData
    );
}

#[test]
fn a_truncated_report_is_refused_rather_than_half_read() {
    let complete = full();
    let cut = complete.len() * 3 / 4;
    let truncated = complete.get(..cut).unwrap();
    let err = cargo_critpath::parse_timing_report(truncated).unwrap_err();
    assert_eq!(
        err,
        ParseError::TruncatedArray {
            name: "UNIT_DATA".to_owned()
        }
    );
    assert!(err.to_string().contains("run it again"), "{err}");
}

#[test]
fn an_entry_missing_a_field_is_named_rather_than_skipped() {
    let text = report(
        0,
        1,
        r#"{"i":0,"name":"a","version":"1.0.0","target":"","features":[],"start":0.0}"#,
    );
    assert_eq!(
        cargo_critpath::parse_timing_report(&text).unwrap_err(),
        ParseError::BadUnitField {
            entry: 0,
            field: "duration",
            expected: "a number of seconds"
        }
    );
}

#[test]
fn an_unusable_duration_is_refused() {
    let text = report(
        0,
        1,
        r#"{"i":0,"name":"a","version":"1.0.0","target":"","features":[],"start":0.0,
            "duration":1e30,"unblocked_units":[],"unblocked_rmeta_units":[],"sections":[]}"#,
    );
    assert!(matches!(
        cargo_critpath::parse_timing_report(&text).unwrap_err(),
        ParseError::DurationOutOfRange { entry: 0, .. }
    ));
}

#[test]
fn a_zero_duration_unit_in_a_clean_build_is_kept() {
    let text = report(
        0,
        1,
        r#"{"i":0,"name":"a","version":"1.0.0","target":"","features":[],"start":0.0,
            "duration":0.0,"unblocked_units":[],"unblocked_rmeta_units":[],"sections":[]}"#,
    );
    let parsed = cargo_critpath::parse_timing_report(&text).unwrap();
    assert_eq!(parsed.units[0].duration, Millis::ZERO);
    assert_eq!(parsed.compilation_span(), Millis::ZERO);
}

#[test]
fn unknown_fields_are_ignored_so_a_newer_cargo_still_parses() {
    let text = report(
        0,
        1,
        r#"{"i":0,"name":"a","version":"1.0.0","mode":"todo","target":"","features":[],
            "start":0.0,"duration":0.5,"unblocked_units":[],"unblocked_rmeta_units":[],
            "sections":[["frontend",{"start":0.0,"end":0.25}],["link",{"start":0.4,"end":0.5}]],
            "rebuild_reason":"source changed","cpu_share":0.87}"#,
    );
    let parsed = cargo_critpath::parse_timing_report(&text).unwrap();
    assert_eq!(parsed.units[0].metadata_at, Some(Millis::new(250)));
}

#[test]
fn a_frontend_section_longer_than_the_unit_is_clamped() {
    let text = report(
        0,
        1,
        r#"{"i":0,"name":"a","version":"1.0.0","target":"","features":[],"start":0.0,
            "duration":0.5,"unblocked_units":[],"unblocked_rmeta_units":[],
            "sections":[["frontend",{"start":0.0,"end":0.9}]]}"#,
    );
    let parsed = cargo_critpath::parse_timing_report(&text).unwrap();
    // Metadata cannot be emitted after the unit that emits it has ended.
    assert_eq!(parsed.units[0].metadata_at, Some(Millis::new(500)));
}

#[test]
fn a_report_with_no_summary_table_is_refused() {
    let text = format!("<html><body><script>const UNIT_DATA = [\n{LIB}\n];</script></body></html>");
    assert_eq!(
        cargo_critpath::parse_timing_report(&text).unwrap_err(),
        ParseError::NoSummaryTable
    );
}

#[test]
fn crlf_line_endings_are_handled() {
    let text = full().replace('\n', "\r\n");
    let parsed = cargo_critpath::parse_timing_report(&text).unwrap();
    assert_eq!(parsed.units.len(), 4);
    assert_eq!(parsed.jobs, Some(10));
}
