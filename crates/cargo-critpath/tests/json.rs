//! The machine-readable output, against the contract in `docs/json-schema.md`.
//!
//! The document is normative, so every claim it makes that can be checked mechanically is checked
//! here, and the whole shape is pinned against a golden file so that a change to it fails the build
//! rather than a consumer's script.

mod common;

use serde_json::Value;

use common::{assert_golden, fixture, run, run_workspace};

/// The command that regenerates the golden document, relative to the repository root.
const REGENERATE: &str = "cargo run -q -p cargo-critpath -- critpath --format json \
    --include-units \
    --log crates/cargo-critpath/tests/fixtures/workspace/verbose.log \
    --timings crates/cargo-critpath/tests/fixtures/workspace/cargo-timing.html \
    --metadata crates/cargo-critpath/tests/fixtures/workspace/cargo-metadata.json \
    > crates/cargo-critpath/tests/golden/analysis.json";

fn document(extra: &[&str]) -> Value {
    let mut args = vec!["--format", "json"];
    args.extend_from_slice(extra);
    let run = run_workspace(&args);
    assert_eq!(run.code(), 0, "{}", run.err);
    serde_json::from_str(&run.out).unwrap_or_else(|e| panic!("{e}\n{}", run.out))
}

fn integer(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("{key} is not an integer in {value}"))
}

fn array<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{key} is not an array in {value}"))
}

/// Every field whose name ends in `_ms` is either an integer or an interval of two integers.
fn check_durations(value: &Value, path: &str, findings: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let here = format!("{path}.{key}");
                if key.ends_with("_ms") {
                    let ok = child.is_i64()
                        || child.is_null()
                        || (child.get("low").and_then(Value::as_i64).is_some()
                            && child.get("high").and_then(Value::as_i64).is_some());
                    if !ok {
                        findings.push(format!("{here} is {child}"));
                    }
                }
                check_durations(child, &here, findings);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                check_durations(child, &format!("{path}[{i}]"), findings);
            }
        }
        _ => {}
    }
}

#[test]
fn the_document_matches_its_golden_file() {
    let run = run_workspace(&["--format", "json", "--include-units"]);
    assert_eq!(run.code(), 0, "{}", run.err);
    assert_golden("analysis.json", &run.out, REGENERATE);
}

#[test]
fn the_document_declares_the_schema_and_tool_versions_separately() {
    let doc = document(&[]);
    assert_eq!(integer(&doc, "schema_version"), 1);
    assert_eq!(
        doc.get("tool_version").and_then(Value::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn every_duration_is_an_integer_number_of_milliseconds() {
    let doc = document(&["--include-units"]);
    let mut findings = Vec::new();
    check_durations(&doc, "", &mut findings);
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

#[test]
fn mean_parallelism_is_the_only_fractional_number_and_is_derived_from_two_integers() {
    let run = run_workspace(&["--format", "json", "--include-units"]);
    let doc: Value = serde_json::from_str(&run.out).unwrap();
    let baseline = doc.get("baseline").unwrap();
    let work = integer(doc.get("graph").unwrap(), "total_work_ms");
    let makespan = integer(baseline, "simulated_makespan_ms");
    // Two decimals, computed here rather than by calling the code that produced the field.
    let hundredths = (work * 100 + makespan / 2) / makespan;
    let expected = format!("{}.{:02}", hundredths / 100, hundredths % 100);
    assert_eq!(
        baseline
            .get("mean_parallelism")
            .map(std::string::ToString::to_string),
        Some(expected)
    );
}

#[test]
fn the_input_section_reports_the_prefix_that_was_excluded() {
    let doc = document(&[]);
    let input = doc.get("input").unwrap();
    let total = integer(input, "reported_total_ms");
    let span = integer(input, "compilation_span_ms");
    assert_eq!(integer(input, "excluded_prefix_ms"), total - span);
    assert_eq!(integer(input, "units_fresh"), 0);
    assert_eq!(integer(input, "jobs"), 4);
    assert_eq!(integer(input, "observed_max_concurrency"), 3);
}

#[test]
fn the_constraint_names_which_of_the_two_bounds_dominates() {
    for (jobs, expected) in [("1", "machine"), ("4", "graph")] {
        let doc = document(&["--jobs", jobs]);
        let baseline = doc.get("baseline").unwrap();
        let critical = integer(baseline, "critical_path_ms");
        let area = integer(baseline, "area_bound_ms");
        assert_eq!(
            baseline.get("constraint").and_then(Value::as_str),
            Some(expected),
            "critical path {critical}, area bound {area}"
        );
        assert_eq!(integer(baseline, "lower_bound_ms"), critical.max(area));
    }
}

#[test]
fn the_critical_path_contributions_sum_to_the_critical_path_length() {
    let doc = document(&[]);
    let expected = integer(doc.get("baseline").unwrap(), "critical_path_ms");
    let sum: i64 = array(&doc, "critical_path")
        .iter()
        .map(|step| integer(step, "contribution_ms"))
        .sum();
    assert_eq!(sum, expected);
}

#[test]
fn the_critical_path_runs_from_a_source_to_the_attaining_unit() {
    let doc = document(&[]);
    let steps = array(&doc, "critical_path");
    let first = steps.first().unwrap();
    assert_eq!(first.get("incoming_edge_kind"), Some(&Value::Null));
    assert_eq!(integer(first, "enter_ms"), 0);
    for step in steps.iter().skip(1) {
        let kind = step.get("incoming_edge_kind").and_then(Value::as_str);
        assert!(matches!(kind, Some("metadata" | "full")), "{step}");
    }
    let last = steps.last().unwrap();
    assert_eq!(
        integer(last, "exit_ms"),
        integer(doc.get("baseline").unwrap(), "critical_path_ms")
    );
}

#[test]
fn the_unit_array_is_emitted_only_when_it_is_asked_for() {
    assert_eq!(document(&[]).get("units"), Some(&Value::Null));

    let doc = document(&["--include-units"]);
    let units = array(&doc, "units");
    assert_eq!(units.len(), integer(doc.get("graph").unwrap(), "units") as usize);
    for (index, unit) in units.iter().enumerate() {
        assert_eq!(integer(unit, "unit"), index as i64);
        let slack = integer(unit, "slack_ms");
        assert_eq!(
            integer(unit, "latest_finish_ms") - integer(unit, "earliest_finish_ms"),
            slack
        );
        assert_eq!(
            unit.get("on_critical_path").and_then(Value::as_bool),
            Some(slack == 0)
        );
    }
}

#[test]
fn scenarios_are_ordered_by_descending_saving() {
    let doc = document(&[]);
    let scenarios = array(&doc, "scenarios");
    assert!(!scenarios.is_empty());
    let savings: Vec<i64> = scenarios
        .iter()
        .map(|s| integer(s.get("saving_ms").unwrap(), "high"))
        .collect();
    let mut sorted = savings.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(savings, sorted, "{scenarios:?}");
}

#[test]
fn every_interval_is_ordered_and_a_split_is_never_exact() {
    let doc = document(&[]);
    let mut splits = 0;
    for scenario in array(&doc, "scenarios") {
        for field in ["critical_path_ms", "saving_ms", "simulated_makespan_ms"] {
            let interval = scenario.get(field).unwrap();
            assert!(
                integer(interval, "low") <= integer(interval, "high"),
                "{field} of {scenario}"
            );
        }
        let exact = scenario.get("exact").and_then(Value::as_bool).unwrap();
        let assumptions = array(scenario, "assumptions");
        if !exact {
            assert!(!assumptions.is_empty(), "{scenario}");
        }
        if scenario.get("kind").and_then(Value::as_str) == Some("split-crate") {
            splits += 1;
            assert!(!exact, "{scenario}");
            assert!(scenario.get("fraction_swept").unwrap().is_object());
            assert_eq!(integer(scenario, "overhead_ms"), 0);
        }
    }
    assert!(splits > 0);
}

#[test]
fn no_split_is_offered_for_a_unit_that_cannot_be_divided() {
    let doc = document(&[]);
    for scenario in array(&doc, "scenarios") {
        if scenario.get("kind").and_then(Value::as_str) != Some("split-crate") {
            continue;
        }
        let target = scenario
            .get("target")
            .and_then(|t| t.get("target"))
            .and_then(Value::as_str)
            .unwrap();
        assert!(!target.starts_with("build-script"), "{scenario}");
    }
}

#[test]
fn the_executor_scan_marks_the_knee_and_leaves_the_critical_path_alone() {
    let doc = document(&[]);
    let critical = integer(doc.get("baseline").unwrap(), "critical_path_ms");
    let mut knees = 0;
    for scenario in array(&doc, "scenarios") {
        if scenario.get("kind").and_then(Value::as_str) != Some("set-executors") {
            continue;
        }
        // The critical path never depends on the executor count.
        let path = scenario.get("critical_path_ms").unwrap();
        assert_eq!(integer(path, "low"), critical);
        assert_eq!(integer(path, "high"), critical);
        if array(scenario, "notes")
            .iter()
            .any(|n| n.as_str().is_some_and(|n| n.contains("knee")))
        {
            knees += 1;
            // 2430ms of work over a 1310ms critical path: two executors is the knee.
            assert_eq!(
                integer(scenario.get("target").unwrap(), "executors"),
                2,
                "{scenario}"
            );
        }
    }
    assert_eq!(knees, 1);
}

#[test]
fn one_timing_report_leaves_the_noise_floor_unknown() {
    let doc = document(&[]);
    let codes: Vec<&str> = array(&doc, "diagnostics")
        .iter()
        .filter_map(|d| d.get("code").and_then(Value::as_str))
        .collect();
    assert!(codes.contains(&"noise-floor-unknown"), "{codes:?}");
    assert!(!codes.contains(&"saving-below-noise-floor"), "{codes:?}");
}

#[test]
fn a_saving_smaller_than_the_measured_spread_is_marked_on_the_scenario_that_earned_it() {
    let run = run(&[
        "--format",
        "json",
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
    let doc: Value = serde_json::from_str(&run.out).unwrap();

    let codes: Vec<&str> = array(&doc, "diagnostics")
        .iter()
        .filter_map(|d| d.get("code").and_then(Value::as_str))
        .collect();
    assert!(!codes.contains(&"noise-floor-unknown"), "{codes:?}");
    assert!(codes.contains(&"saving-below-noise-floor"), "{codes:?}");

    // The spread between the two reports is 130ms, so a scenario is marked exactly when the most
    // it could save is under that.
    for scenario in array(&doc, "scenarios") {
        if scenario.get("kind").and_then(Value::as_str) != Some("split-crate") {
            continue;
        }
        let best = integer(scenario.get("saving_ms").unwrap(), "high");
        let marked = array(scenario, "notes")
            .iter()
            .any(|n| n.as_str().is_some_and(|n| n.contains("spread")));
        assert_eq!(marked, best < 130, "{scenario}");
    }
}

#[test]
fn diagnostics_are_ordered_by_severity_and_then_by_code() {
    let doc = document(&[]);
    let keys: Vec<(bool, String)> = array(&doc, "diagnostics")
        .iter()
        .map(|d| {
            let severity = d.get("severity").and_then(Value::as_str).unwrap_or("");
            let code = d.get("code").and_then(Value::as_str).unwrap_or("");
            (severity != "error", code.to_owned())
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn the_output_is_the_same_every_time() {
    let first = run_workspace(&["--format", "json", "--include-units"]);
    let second = run_workspace(&["--format", "json", "--include-units"]);
    assert_eq!(first.out, second.out);
}
