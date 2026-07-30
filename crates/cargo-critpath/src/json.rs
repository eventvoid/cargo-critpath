//! The machine-readable document, against the contract in [`docs/json-schema.md`](https://github.com/eventvoid/cargo-critpath/blob/main/docs/json-schema.md).
//!
//! The document is written out field by field rather than serialised from a map, for two reasons.
//! The contract fixes the order of every array, and a `serde_json` object is a sorted map whose key
//! order is its own business; and writing it out makes the shape of the contract visible in one
//! place, next to the document that defines it.

use std::io::{self, Write};

use critpath_core::{EdgeKind, Millis, Policy, UnitKind};
use serde_json::Value;

use crate::findings::{Findings, Label, Range, Saving, Scenario, Step, Target, UnitFacts};
use crate::input::{Diagnostic, Severity};

/// The version of the document's shape. Bumped only by a breaking change to it.
const SCHEMA_VERSION: u32 = 1;

/// Writes the analysis as one JSON object.
///
/// # Errors
///
/// Any [`io::Error`] from the destination.
pub(crate) fn write(
    out: &mut dyn Write,
    findings: &Findings,
    include_units: bool,
) -> io::Result<()> {
    writeln!(out, "{{")?;
    writeln!(out, "  \"schema_version\": {SCHEMA_VERSION},")?;
    writeln!(
        out,
        "  \"tool_version\": {},",
        quoted(env!("CARGO_PKG_VERSION"))
    )?;
    input(out, findings)?;
    graph(out, findings)?;
    baseline(out, findings)?;
    critical_path(out, findings)?;
    units(out, findings, include_units)?;
    scenarios(out, findings)?;
    diagnostics(out, &findings.diagnostics)?;
    writeln!(out, "}}")
}

fn input(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    let meta = &findings.input;
    writeln!(out, "  \"input\": {{")?;
    writeln!(
        out,
        "    \"cargo_version\": {},",
        optional_text(meta.cargo_version.as_deref())
    )?;
    writeln!(
        out,
        "    \"rustc_version\": {},",
        optional_text(meta.rustc_version.as_deref())
    )?;
    writeln!(
        out,
        "    \"host\": {},",
        optional_text(meta.host.as_deref())
    )?;
    writeln!(
        out,
        "    \"target\": {},",
        optional_text(meta.target.as_deref())
    )?;
    writeln!(
        out,
        "    \"profile\": {},",
        optional_text(meta.profile.as_deref())
    )?;
    writeln!(out, "    \"jobs\": {},", optional_count(meta.jobs))?;
    writeln!(
        out,
        "    \"observed_max_concurrency\": {},",
        optional_count(meta.observed_max_concurrency)
    )?;
    writeln!(out, "    \"units_total\": {},", meta.units_total)?;
    writeln!(out, "    \"units_fresh\": {},", meta.units_fresh)?;
    writeln!(
        out,
        "    \"compilation_span_ms\": {},",
        meta.compilation_span.get()
    )?;
    writeln!(
        out,
        "    \"reported_total_ms\": {},",
        match meta.reported_total {
            Some(total) => total.get().to_string(),
            None => "null".to_owned(),
        }
    )?;
    writeln!(
        out,
        "    \"excluded_prefix_ms\": {}",
        meta.excluded_prefix.get()
    )?;
    writeln!(out, "  }},")
}

fn graph(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    let facts = &findings.graph;
    writeln!(out, "  \"graph\": {{")?;
    writeln!(out, "    \"units\": {},", facts.units)?;
    writeln!(out, "    \"edges\": {},", facts.edges)?;
    writeln!(out, "    \"metadata_edges\": {},", facts.metadata_edges)?;
    writeln!(out, "    \"total_work_ms\": {}", facts.total_work.get())?;
    writeln!(out, "  }},")
}

fn baseline(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    let base = &findings.baseline;
    writeln!(out, "  \"baseline\": {{")?;
    writeln!(out, "    \"executors\": {},", base.executors)?;
    writeln!(
        out,
        "    \"critical_path_ms\": {},",
        base.critical_path.get()
    )?;
    writeln!(out, "    \"area_bound_ms\": {},", base.area_bound.get())?;
    writeln!(out, "    \"lower_bound_ms\": {},", base.lower_bound.get())?;
    writeln!(
        out,
        "    \"simulated_makespan_ms\": {},",
        base.makespan.get()
    )?;
    writeln!(out, "    \"policy\": {},", quoted(policy(base.policy)))?;
    writeln!(
        out,
        "    \"constraint\": {},",
        quoted(if base.graph_constrained {
            "graph"
        } else {
            "machine"
        })
    )?;
    writeln!(
        out,
        "    \"mean_parallelism\": {}",
        decimal(base.mean_parallelism)
    )?;
    writeln!(out, "  }},")
}

fn critical_path(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    if findings.path.is_empty() {
        return writeln!(out, "  \"critical_path\": [],");
    }
    writeln!(out, "  \"critical_path\": [")?;
    for (position, step) in findings.path.iter().enumerate() {
        step_object(out, step, position.saturating_add(1) == findings.path.len())?;
    }
    writeln!(out, "  ],")
}

fn step_object(out: &mut dyn Write, step: &Step, last: bool) -> io::Result<()> {
    writeln!(out, "    {{")?;
    writeln!(out, "      \"unit\": {},", step.unit)?;
    writeln!(out, "      \"label\": {},", label(&step.label))?;
    writeln!(out, "      \"kind\": {},", quoted(kind(step.kind)))?;
    writeln!(out, "      \"enter_ms\": {},", step.enter.get())?;
    writeln!(out, "      \"exit_ms\": {},", step.exit.get())?;
    writeln!(
        out,
        "      \"contribution_ms\": {},",
        step.contribution.get()
    )?;
    writeln!(
        out,
        "      \"incoming_edge_kind\": {},",
        match step.incoming {
            Some(EdgeKind::Metadata) => quoted("metadata"),
            Some(EdgeKind::Full) => quoted("full"),
            None => "null".to_owned(),
        }
    )?;
    writeln!(out, "      \"total_ms\": {},", step.total.get())?;
    writeln!(
        out,
        "      \"metadata_at_ms\": {}",
        optional_millis(step.metadata_at)
    )?;
    writeln!(out, "    }}{}", separator(last))
}

fn units(out: &mut dyn Write, findings: &Findings, include: bool) -> io::Result<()> {
    if !include {
        // Absent rather than empty: an empty array would say the build has no units.
        return writeln!(out, "  \"units\": null,");
    }
    if findings.units.is_empty() {
        return writeln!(out, "  \"units\": [],");
    }
    writeln!(out, "  \"units\": [")?;
    for (position, unit) in findings.units.iter().enumerate() {
        unit_object(
            out,
            unit,
            position.saturating_add(1) == findings.units.len(),
        )?;
    }
    writeln!(out, "  ],")
}

fn unit_object(out: &mut dyn Write, unit: &UnitFacts, last: bool) -> io::Result<()> {
    writeln!(out, "    {{")?;
    writeln!(out, "      \"unit\": {},", unit.unit)?;
    writeln!(out, "      \"label\": {},", label(&unit.label))?;
    writeln!(out, "      \"kind\": {},", quoted(kind(unit.kind)))?;
    writeln!(out, "      \"total_ms\": {},", unit.total.get())?;
    writeln!(
        out,
        "      \"metadata_at_ms\": {},",
        optional_millis(unit.metadata_at)
    )?;
    writeln!(
        out,
        "      \"earliest_start_ms\": {},",
        unit.earliest_start.get()
    )?;
    writeln!(
        out,
        "      \"earliest_finish_ms\": {},",
        unit.earliest_finish.get()
    )?;
    writeln!(
        out,
        "      \"latest_finish_ms\": {},",
        unit.latest_finish.get()
    )?;
    writeln!(out, "      \"slack_ms\": {},", unit.slack.get())?;
    writeln!(
        out,
        "      \"on_critical_path\": {},",
        unit.on_critical_path
    )?;
    writeln!(out, "      \"dependencies\": {},", unit.dependencies)?;
    writeln!(out, "      \"dependents\": {}", unit.dependents)?;
    writeln!(out, "    }}{}", separator(last))
}

fn scenarios(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    if findings.scenarios.is_empty() {
        return writeln!(out, "  \"scenarios\": [],");
    }
    writeln!(out, "  \"scenarios\": [")?;
    for (position, scenario) in findings.scenarios.iter().enumerate() {
        scenario_object(
            out,
            scenario,
            position.saturating_add(1) == findings.scenarios.len(),
        )?;
    }
    writeln!(out, "  ],")
}

fn scenario_object(out: &mut dyn Write, scenario: &Scenario, last: bool) -> io::Result<()> {
    writeln!(out, "    {{")?;
    writeln!(out, "      \"kind\": {},", quoted(scenario.kind))?;
    writeln!(out, "      \"target\": {},", target(&scenario.target))?;
    writeln!(out, "      \"exact\": {},", scenario.exact)?;
    writeln!(
        out,
        "      \"fraction_swept\": {},",
        match scenario.fraction_swept {
            Some((low, high)) => format!(
                "{{\"low\": {}, \"high\": {}}}",
                decimal(u64::from(low)),
                decimal(u64::from(high))
            ),
            None => "null".to_owned(),
        }
    )?;
    writeln!(
        out,
        "      \"overhead_ms\": {},",
        optional_millis(scenario.overhead)
    )?;
    strings(out, "assumptions", &scenario.assumptions, false)?;
    writeln!(
        out,
        "      \"critical_path_ms\": {},",
        range(scenario.critical_path)
    )?;
    writeln!(out, "      \"saving_ms\": {},", saving(scenario.saving))?;
    writeln!(
        out,
        "      \"simulated_makespan_ms\": {},",
        range(scenario.makespan)
    )?;
    writeln!(out, "      \"anomaly\": {},", scenario.anomaly)?;
    strings(out, "notes", &scenario.notes, true)?;
    writeln!(out, "    }}{}", separator(last))
}

fn diagnostics(out: &mut dyn Write, diagnostics: &[Diagnostic]) -> io::Result<()> {
    if diagnostics.is_empty() {
        return writeln!(out, "  \"diagnostics\": []");
    }
    writeln!(out, "  \"diagnostics\": [")?;
    for (position, diagnostic) in diagnostics.iter().enumerate() {
        let severity = match diagnostic.severity {
            Severity::Warning => "warning",
        };
        writeln!(out, "    {{")?;
        writeln!(out, "      \"severity\": {},", quoted(severity))?;
        writeln!(out, "      \"code\": {},", quoted(diagnostic.code))?;
        writeln!(out, "      \"message\": {}", quoted(&diagnostic.message))?;
        writeln!(
            out,
            "    }}{}",
            separator(position.saturating_add(1) == diagnostics.len())
        )?;
    }
    writeln!(out, "  ]")
}

/// One array of strings inside a scenario, `[]` on one line when it is empty.
fn strings(out: &mut dyn Write, name: &str, items: &[String], last: bool) -> io::Result<()> {
    if items.is_empty() {
        return writeln!(out, "      \"{name}\": []{}", separator(last));
    }
    writeln!(out, "      \"{name}\": [")?;
    for (position, item) in items.iter().enumerate() {
        writeln!(
            out,
            "        {}{}",
            quoted(item),
            separator(position.saturating_add(1) == items.len())
        )?;
    }
    writeln!(out, "      ]{}", separator(last))
}

fn separator(last: bool) -> &'static str {
    if last {
        ""
    } else {
        ","
    }
}

fn label(label: &Label) -> String {
    format!(
        "{{\"package\": {}, \"version\": {}, \"target\": {}}}",
        quoted(&label.package),
        quoted(&label.version),
        quoted(&label.target)
    )
}

fn target(target: &Target) -> String {
    match target {
        Target::Unit(unit) => label(unit),
        Target::Executors(count) => format!("{{\"executors\": {count}}}"),
    }
}

fn range(range: Range) -> String {
    format!(
        "{{\"low\": {}, \"high\": {}}}",
        range.low.get(),
        range.high.get()
    )
}

fn saving(saving: Saving) -> String {
    format!("{{\"low\": {}, \"high\": {}}}", saving.low, saving.high)
}

fn quoted(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

fn optional_text(text: Option<&str>) -> String {
    match text {
        Some(text) => quoted(text),
        None => "null".to_owned(),
    }
}

fn optional_count(count: Option<u32>) -> String {
    match count {
        Some(count) => count.to_string(),
        None => "null".to_owned(),
    }
}

fn optional_millis(value: Option<Millis>) -> String {
    match value {
        Some(value) => value.get().to_string(),
        None => "null".to_owned(),
    }
}

fn policy(policy: Policy) -> &'static str {
    match policy {
        Policy::LongestPath => "longest-path",
        _ => "cargo-like",
    }
}

fn kind(kind: UnitKind) -> &'static str {
    kind.as_str()
}

/// A count of hundredths as a decimal with exactly two places.
///
/// Written out rather than formatted from a float: `mean_parallelism` is the one non-integer field
/// in the document, and it must not depend on how a platform renders a `f64`.
#[allow(clippy::integer_division)]
fn decimal(hundredths: u64) -> String {
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}
