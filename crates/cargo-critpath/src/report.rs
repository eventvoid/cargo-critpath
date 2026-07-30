//! The human-readable report.
//!
//! The order of the sections is the argument the tool makes. The critical path and the two bounds
//! come first, because they hold under every scheduling policy; the simulated makespan comes after
//! them and is labelled an estimate. Units are ranked by what they contribute to the critical path
//! rather than by membership of it, because membership at the margin flips between runs of the same
//! build. Nothing that cannot be acted on is presented as a recommendation.

use std::io::{self, Write};

use critpath_core::{EdgeKind, Policy, UnitKind};

use crate::findings::{Direction, Findings, Noise, Scenario, Target};
use crate::input::Severity;

/// How wide prose is wrapped.
const WIDTH: usize = 100;

/// Writes the analysis as a report meant to be read.
///
/// # Errors
///
/// Any [`io::Error`] from the destination.
pub(crate) fn write(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    writeln!(out, "cargo-critpath {}", env!("CARGO_PKG_VERSION"))?;
    build(out, findings)?;
    bounds(out, findings)?;
    contributors(out, findings)?;
    path(out, findings)?;
    splits(out, findings)?;
    noise(out, findings)?;
    executors(out, findings)?;
    diagnostics(out, findings)
}

fn build(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    let meta = &findings.input;
    let graph = &findings.graph;
    writeln!(out, "\nbuild")?;
    writeln!(
        out,
        "  {} units and {} of work, over {} edges, {} of them satisfied by metadata",
        graph.units, graph.total_work, graph.edges, graph.metadata_edges
    )?;
    writeln!(
        out,
        "  {} profile, {}, target {}",
        meta.profile.as_deref().unwrap_or("unknown"),
        meta.rustc_version.as_deref().unwrap_or("unknown rustc"),
        meta.target.as_deref().unwrap_or("unknown")
    )?;
    match (meta.jobs, meta.observed_max_concurrency) {
        (Some(jobs), Some(peak)) => writeln!(
            out,
            "  analysed for {} executor{}; the build ran with -j {jobs} and reached a peak of {peak} units at once",
            findings.baseline.executors,
            plural(findings.baseline.executors.get())
        )?,
        _ => writeln!(
            out,
            "  analysed for {} executor{}; the timing report does not record what the build ran with",
            findings.baseline.executors,
            plural(findings.baseline.executors.get())
        )?,
    }
    if let Some(total) = meta.reported_total {
        writeln!(
            out,
            "  {total} reported in total, {} of it outside the unit model",
            meta.excluded_prefix
        )?;
        paragraph(
            out,
            "    ",
            "Fetching, resolution and anything else before the first unit started. Reported here \
             rather than folded into the numbers below, which are about compilation only.",
        )?;
    }
    Ok(())
}

fn bounds(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    let base = &findings.baseline;
    writeln!(out, "\nbounds")?;
    writeln!(
        out,
        "  critical path      {:>10}   holds under every scheduling policy",
        base.critical_path.to_string()
    )?;
    writeln!(
        out,
        "  total work / {:<5} {:>10}",
        base.executors,
        base.area_bound.to_string()
    )?;
    writeln!(
        out,
        "  lower bound        {:>10}   constraint: {}",
        base.lower_bound.to_string(),
        if base.graph_constrained {
            "graph, the shape of the build is what holds it back"
        } else {
            "machine, there is more work here than executors to run it"
        }
    )?;
    writeln!(
        out,
        "  simulated makespan {:>10}   an estimate under the {} policy, not a bound",
        base.makespan.to_string(),
        policy(base.policy)
    )?;
    writeln!(
        out,
        "  mean parallelism   {:>10}   total work over the simulated makespan",
        decimal(base.mean_parallelism)
    )?;

    if !base.graph_constrained {
        writeln!(out)?;
        paragraph(
            out,
            "  ",
            &format!(
                "The machine is the constraint, not the graph. Restructuring the build will not \
                 shorten it much below {} until it has at least {} executors, and the scenarios \
                 below act on a critical path this build is not waiting on. They are still \
                 computed, because the knee is what says how many executors would be worth having.",
                base.area_bound, findings.knee
            ),
        )?;
    }
    Ok(())
}

fn contributors(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    writeln!(out, "\nunits by contribution to the critical path")?;
    paragraph(
        out,
        "  ",
        &format!(
            "What each unit adds to the {}. Ranked by contribution rather than by membership of \
             the path, because the head of the path flips between runs of the same build and the \
             contributions do not. They sum to the critical path exactly.",
            findings.baseline.critical_path
        ),
    )?;
    for index in findings.ranked.iter().take(findings.top) {
        let Some(unit) = findings.units.get(*index) else {
            continue;
        };
        writeln!(
            out,
            "    {:>7}  {}  [{}]",
            unit.contribution.to_string(),
            unit.label.display(),
            unit.kind
        )?;
    }
    remainder(
        out,
        findings.ranked.len(),
        findings.top,
        "contributing units",
    )
}

fn path(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    writeln!(out, "\nthe critical path, in order")?;
    writeln!(out, "      enter       exit       adds  unit")?;
    for step in findings.path.iter().take(findings.top) {
        writeln!(
            out,
            "  {:>9}  {:>9}  {:>9}  {}{}",
            step.enter.to_string(),
            step.exit.to_string(),
            step.contribution.to_string(),
            step.label.display(),
            match step.incoming {
                Some(EdgeKind::Metadata) => "  (entered on the metadata of the unit above)",
                Some(EdgeKind::Full) => "",
                None => "",
            }
        )?;
    }
    remainder(out, findings.path.len(), findings.top, "units on the path")
}

fn splits(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    writeln!(out, "\nwhat splitting a crate could do")?;
    paragraph(
        out,
        "  ",
        "A split cannot be modelled exactly, because which half each dependent would need is not \
         knowable without performing it. Each figure below is a range over a sweep of the division \
         fraction from 0.25 to 0.75 under both dependent assumptions. At an even division those \
         two assumptions give the same critical path, so what the range is really made of is the \
         sweep over the division. Each half is charged 0ms of per-crate overhead, because nobody \
         has measured what a real split costs and inventing a number would be worse than charging \
         nothing and saying so.",
    )?;

    let splits: Vec<&Scenario> = findings
        .scenarios
        .iter()
        .filter(|scenario| scenario.kind == "split-crate")
        .collect();
    if splits.is_empty() {
        let divisible = findings
            .ranked
            .iter()
            .filter_map(|index| findings.units.get(*index))
            .any(|unit| unit.splittable);
        writeln!(
            out,
            "  {}",
            if divisible {
                "None shown, because --top is 0."
            } else {
                "Nothing on the critical path can be divided."
            }
        )?;
    }
    for scenario in splits {
        let Target::Unit(label) = &scenario.target else {
            continue;
        };
        writeln!(out, "\n  {}", label.display())?;
        writeln!(
            out,
            "    critical path  {:>9} to {:<9}  against {} as it stands",
            scenario.critical_path.low.to_string(),
            scenario.critical_path.high.to_string(),
            findings.baseline.critical_path
        )?;
        let low = format!("{}ms", scenario.saving.low);
        writeln!(
            out,
            "    saving         {low:>9} to {}ms",
            scenario.saving.high
        )?;
        writeln!(
            out,
            "    makespan       {:>9} to {:<9}  estimate, not a bound",
            scenario.makespan.low.to_string(),
            scenario.makespan.high.to_string()
        )?;
        for note in &scenario.notes {
            paragraph(out, "    ", note)?;
        }
    }

    if !findings.unsplittable.is_empty() {
        writeln!(out)?;
        for index in &findings.unsplittable {
            let Some(unit) = findings.units.get(*index) else {
                continue;
            };
            let reason = match unit.kind {
                UnitKind::BuildScriptRun => {
                    "one execution of one program is a single indivisible step, so there is no \
                     split of it to model"
                }
                _ => {
                    "Cargo generates a build script's compilation from the script itself, so \
                     dividing it is not a change to this graph that a workspace could make"
                }
            };
            paragraph(
                out,
                "  ",
                &format!(
                    "Not offered: {} adds {} to the critical path, but {reason}.",
                    unit.label.display(),
                    unit.contribution
                ),
            )?;
        }
    }
    Ok(())
}

fn noise(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    writeln!(out, "\nnoise floor")?;
    match &findings.noise {
        Noise::Unknown => {
            writeln!(
                out,
                "  Only one timing report was supplied, so the run-to-run spread is unknown."
            )?;
            paragraph(
                out,
                "  ",
                "No saving above can be checked against it, and this tool will not invent a \
                 threshold. Pass --timings once per clean build to measure it. On four real \
                 projects the spread ran from 2.3% to 11.8% of the critical path, which is wide \
                 enough to swallow a recommendation whole.",
            )?;
        }
        Noise::Measured { spread, samples } => {
            let lengths: Vec<String> = samples.iter().map(ToString::to_string).collect();
            writeln!(
                out,
                "  {} timing reports gave critical paths of {}: a spread of {spread}.",
                samples.len(),
                lengths.join(", ")
            )?;
            paragraph(
                out,
                "  ",
                "A saving smaller than that is not distinguishable from a repeat of the same \
                 build, and is marked above rather than removed.",
            )?;
        }
    }
    Ok(())
}

fn executors(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    writeln!(out, "\nexecutors")?;
    writeln!(
        out,
        "  The knee is at {} executors: from there on the critical path dominates.",
        findings.knee
    )?;
    paragraph(
        out,
        "  ",
        "Beyond the knee, total work divided between the executors falls under the critical path, \
         so more of them cannot shorten this build. The knee is decided by the critical path and by \
         total work, neither of which depends on the executor count, so it is not affected by the \
         extrapolation error below in the way an absolute time is. It does still move with ordinary \
         run-to-run variation, because total work does: repeated clean builds of ripgrep put it at \
         6 and at 7. Treat it as a small range, not a threshold.",
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "     J   work / J  lower bound   simulated   constraint  reliability"
    )?;
    for point in &findings.scan {
        writeln!(
            out,
            "  {:>4}  {:>9}  {:>11}  {:>10}   {:<10}  {}{}",
            point.executors,
            point.area_bound.to_string(),
            point.lower_bound.to_string(),
            point.makespan.to_string(),
            if point.graph_constrained {
                "graph"
            } else {
                "machine"
            },
            if point.is_knee { "knee, " } else { "" },
            match point.direction {
                Direction::Measured => "weights measured here",
                Direction::Optimistic => "optimistic",
                Direction::Pessimistic => "pessimistic",
                Direction::Unknown => "unknown",
            }
        )?;
    }
    writeln!(out)?;
    match findings.input.jobs {
        Some(jobs) => paragraph(
            out,
            "  ",
            &format!(
                "The per-unit weights come from one build at -j {jobs} and are not invariant to \
                 it. Measured on two real projects, total unit work fell 23% to 30% between ten \
                 executors and two. Rows above -j {jobs} are therefore optimistic and rows below \
                 it pessimistic, by an amount this tool cannot correct for."
            ),
        )?,
        None => paragraph(
            out,
            "  ",
            "The timing report does not record what executor count the build ran with, so which \
             rows are optimistic and which pessimistic cannot be said.",
        )?,
    }
    paragraph(
        out,
        "  ",
        "The simulated column is not monotone in the executor count. Adding an executor can \
         lengthen a greedy schedule, so a row longer than the one above it is a property of list \
         scheduling and not an error.",
    )
}

fn diagnostics(out: &mut dyn Write, findings: &Findings) -> io::Result<()> {
    if findings.diagnostics.is_empty() {
        return Ok(());
    }
    writeln!(out, "\nnotes")?;
    for diagnostic in &findings.diagnostics {
        let severity = match diagnostic.severity {
            Severity::Warning => "warning",
        };
        paragraph(
            out,
            "  ",
            &format!("{severity} [{}] {}", diagnostic.code, diagnostic.message),
        )?;
    }
    Ok(())
}

fn remainder(out: &mut dyn Write, total: usize, shown: usize, what: &str) -> io::Result<()> {
    if total > shown {
        writeln!(
            out,
            "    and {} more {what}, not shown; raise --top to see them",
            total.saturating_sub(shown)
        )?;
    }
    Ok(())
}

/// Writes `text` wrapped to [`WIDTH`] columns, every line prefixed with `indent`.
///
/// Breaking is at whitespace only, and a word longer than the line gets a line of its own rather
/// than being cut, so no identifier is ever silently truncated.
fn paragraph(out: &mut dyn Write, indent: &str, text: &str) -> io::Result<()> {
    let limit = WIDTH.saturating_sub(indent.len());
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len().saturating_add(word.len()).saturating_add(1) > limit {
            writeln!(out, "{indent}{line}")?;
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        writeln!(out, "{indent}{line}")?;
    }
    Ok(())
}

/// The plural `s`, for a count written next to a noun.
fn plural(count: u32) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn policy(policy: Policy) -> &'static str {
    match policy {
        Policy::LongestPath => "longest-path",
        _ => "cargo-like",
    }
}

/// A count of hundredths as a decimal with exactly two places.
#[allow(clippy::integer_division)]
fn decimal(hundredths: u64) -> String {
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}
