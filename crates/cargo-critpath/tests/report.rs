//! What the human-readable report says, and what it must never say.
//!
//! The wording is part of the product: a number printed without its caveat is the failure this
//! project exists to avoid. The golden file pins the whole thing so that a change to it is
//! deliberate; the tests around it pin the claims that must survive any rewording.

mod common;

use common::{assert_golden, fixture, run, run_workspace};

/// The command that regenerates the golden human report, relative to the repository root.
const REGENERATE: &str = "cargo run -q -p cargo-critpath -- critpath \
    --log crates/cargo-critpath/tests/fixtures/workspace/verbose.log \
    --timings crates/cargo-critpath/tests/fixtures/workspace/cargo-timing.html \
    --metadata crates/cargo-critpath/tests/fixtures/workspace/cargo-metadata.json \
    > crates/cargo-critpath/tests/golden/report.txt";

fn line_containing<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line contains {needle:?} in:\n{text}"))
}

#[test]
fn the_human_report_matches_its_golden_file() {
    let run = run_workspace(&[]);
    assert_eq!(run.code(), 0, "{}", run.err);
    assert_golden("report.txt", &run.out, REGENERATE);
}

#[test]
fn the_critical_path_and_the_bound_come_before_the_simulated_makespan() {
    let run = run_workspace(&[]);
    let critical = run.out.find("critical path").unwrap_or(usize::MAX);
    let bound = run.out.find("lower bound").unwrap_or(usize::MAX);
    let simulated = run.out.find("simulated").unwrap_or(usize::MAX);
    assert!(
        critical < simulated && bound < simulated,
        "the estimate is presented before the guaranteed numbers:\n{}",
        run.out
    );
}

#[test]
fn a_graph_limited_run_names_the_graph_as_the_constraint() {
    // 2430ms of work over four executors is 608ms, well under the 1310ms critical path.
    let run = run_workspace(&["--jobs", "4"]);
    let line = line_containing(&run.out, "constraint");
    assert!(line.contains("graph"), "{line}");
    assert!(!line.contains("machine"), "{line}");
}

#[test]
fn a_machine_limited_run_says_restructuring_will_not_help() {
    // 2430ms of work on one executor is 2430ms, well over the 1310ms critical path.
    let run = run_workspace(&["--jobs", "1"]);
    let line = line_containing(&run.out, "constraint");
    assert!(line.contains("machine"), "{line}");
    assert!(
        run.out.contains("will not"),
        "a machine-limited build must say plainly that restructuring will not help:\n{}",
        run.out
    );
}

#[test]
fn units_are_ranked_by_contribution_rather_than_by_membership() {
    let run = run_workspace(&[]);
    // mid contributes 520ms and the leaf build-script run 400ms, so mid must come first even
    // though the run enters the path earlier.
    let mid = run.out.find("520ms").unwrap_or(usize::MAX);
    let script = run.out.find("400ms").unwrap_or(usize::MAX);
    assert!(mid < script, "{}", run.out);
    assert!(run.out.contains("contribut"), "{}", run.out);
}

#[test]
fn one_timing_report_leaves_the_noise_floor_unknown_and_says_so() {
    let run = run_workspace(&[]);
    let line = line_containing(&run.out, "spread");
    assert!(line.contains("one timing report"), "{line}");
    assert!(
        !run.out.contains("below the"),
        "nothing can be below a floor that was never measured:\n{}",
        run.out
    );
}

#[test]
fn several_timing_reports_measure_the_spread_and_mark_the_savings_under_it() {
    let run = run(&[
        "--log",
        &fixture("workspace/verbose.log"),
        "--timings",
        &fixture("workspace/cargo-timing.html"),
        "--timings",
        &fixture("workspace/cargo-timing-slower.html"),
        "--metadata",
        &fixture("workspace/cargo-metadata.json"),
    ]);
    assert_eq!(run.code(), 0, "{}", run.err);
    // The critical path is 1310ms in the captured report and 1440ms in the derived one.
    assert!(run.out.contains("130ms"), "{}", run.out);
    assert!(run.out.contains("below the"), "{}", run.out);
}

#[test]
fn a_build_script_run_is_never_offered_as_something_to_split() {
    let run = run_workspace(&[]);
    assert!(
        run.out.contains("cannot be divided"),
        "the second largest contributor is a build-script run, and the report has to say why it \
         is not offered rather than leaving it out silently:\n{}",
        run.out
    );
}

#[test]
fn a_split_is_reported_as_a_range_with_its_assumptions() {
    let run = run_workspace(&[]);
    assert!(run.out.contains(" to "), "{}", run.out);
    assert!(run.out.contains("0.25"), "{}", run.out);
    assert!(run.out.contains("0.75"), "{}", run.out);
    assert!(
        run.out.contains("even division"),
        "the report must say that the two dependent assumptions coincide at an even division:\n{}",
        run.out
    );
}

#[test]
fn the_executor_scan_leads_with_the_knee_and_qualifies_the_absolute_times() {
    let run = run_workspace(&[]);
    // 2430ms of work over a 1310ms critical path: from two executors on, the path dominates.
    let knee = line_containing(&run.out, "nee");
    assert!(knee.contains('2'), "{knee}");
    assert!(run.out.contains("optimistic"), "{}", run.out);
    assert!(run.out.contains("pessimistic"), "{}", run.out);
    assert!(
        run.out.contains("monotone"),
        "adding an executor can lengthen a schedule, and the scan must not read as a curve:\n{}",
        run.out
    );
}

#[test]
fn the_excluded_prefix_is_reported_separately_from_the_compilation() {
    // Cargo reported 1400ms in total for a compilation span of 1310ms.
    let run = run_workspace(&[]);
    let line = line_containing(&run.out, "outside the unit model");
    assert!(line.contains("90ms"), "{line}");
    assert!(line.contains("1400ms"), "{line}");
}

#[test]
fn the_observed_peak_concurrency_appears_beside_the_executor_count() {
    // The build ran with -j 4 and Cargo reached three units at once.
    let run = run_workspace(&[]);
    let line = line_containing(&run.out, "peak");
    assert!(line.contains('3'), "{line}");
    assert!(line.contains('4'), "{line}");
}

#[test]
fn the_top_option_limits_how_much_is_listed() {
    let long = run_workspace(&["--top", "10"]);
    let short = run_workspace(&["--top", "1"]);
    assert!(short.out.lines().count() < long.out.lines().count());
}
