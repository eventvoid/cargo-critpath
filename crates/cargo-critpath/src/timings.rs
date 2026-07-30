//! Reading per-unit weights out of `cargo build --timings`.
//!
//! There is no JSON timing output on any channel: `--timings` is a valueless flag and
//! `--timings=json` is rejected. The report is HTML with JavaScript data islands embedded in it,
//! and the array literals in those islands are valid JSON, so they are located by their assignment
//! and handed to `serde_json`. No HTML parser is involved, and none would make an undocumented
//! format less fragile.

use serde_json::Value;

use critpath_core::Millis;

use crate::error::ParseError;

/// What the timing report says a unit is.
///
/// The report's `target` field is a display string rather than structured data, and it renders every
/// library target as the empty string. A plain rlib, a procedural macro, a `cdylib` and a `dylib`
/// are therefore indistinguishable here, which is one of the two reasons the verbose log is needed
/// as well.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum TimingTarget {
    /// A library of some sort.
    Library,
    /// Compiling a build script.
    BuildScriptCompile,
    /// Running a build script that was already compiled.
    BuildScriptRun,
    /// A named target, such as `` rg "bin" ``.
    Named {
        /// The target's own name, which for a binary is the binary's name rather than the package's.
        name: String,
        /// The target kind Cargo printed, such as `bin`, `test` or `bench`.
        kind: String,
    },
    /// Something this parser has not seen, kept verbatim.
    Other(String),
}

impl TimingTarget {
    /// A short name for the target, for display and for the machine-readable output's label.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Library => "lib",
            Self::BuildScriptCompile => "build-script",
            Self::BuildScriptRun => "build-script (run)",
            Self::Named { name, .. } => name,
            Self::Other(text) => text,
        }
    }
}

/// One entry of the report's `UNIT_DATA` array.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TimingUnit {
    /// The report's own `i`, which is the unit's index in Cargo's sorted unit list. The array is
    /// ordered by start time, so this is a permutation rather than the position, and the
    /// `unblocked_*` fields refer to it.
    pub index: usize,
    /// Position in the array, which is start order.
    pub position: usize,
    /// The package's name, which the report states authoritatively.
    pub package: String,
    /// The package's version.
    pub version: String,
    /// What sort of target this is, as far as the report can say.
    pub target: TimingTarget,
    /// The features the unit was compiled with, sorted. These separate two instances of one package
    /// at one version when the rest of the report cannot.
    pub features: Vec<String>,
    /// When the unit started, measured from the beginning of the build.
    pub start: Millis,
    /// How long the unit took.
    pub duration: Millis,
    /// How far into the unit its `frontend` section ended, which is when metadata became available,
    /// clamped to the unit's own duration.
    pub metadata_at: Option<Millis>,
    /// Units this one unblocked by finishing. **Not the dependency graph**: only the units for which
    /// this one happened to be the last remaining blocker, which covered 51% of the real edges on a
    /// measured 28-unit build. Useful as a cross-check, useless as a source.
    pub unblocked: Vec<usize>,
    /// Units this one unblocked by emitting its metadata. Partial in the same way.
    pub unblocked_rmeta: Vec<usize>,
}

/// Everything one timing report described.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TimingReport {
    /// Every unit, in the order the report listed them, which is start order.
    pub units: Vec<TimingUnit>,
    /// Units the build reused from the cache. The analysis requires this to be zero.
    pub fresh_units: usize,
    /// Units the build actually compiled.
    pub dirty_units: usize,
    /// Units in the build.
    pub total_units: usize,
    /// The `-j` the build ran with, from `Max concurrency: N (jobs=J ncpu=C)`.
    pub jobs: Option<u32>,
    /// The machine's processor count, from the same row.
    pub cpus: Option<u32>,
    /// The peak number of units Cargo actually had in flight. This can exceed `jobs`, because a
    /// build-script execution is not a compiler process and takes no compile token.
    pub max_concurrency: Option<u32>,
    /// The profile the build used.
    pub profile: Option<String>,
    /// The `rustc` version, as the report's header states it.
    pub rustc_version: Option<String>,
    /// The host triple.
    pub host: Option<String>,
    /// The target triple.
    pub target: Option<String>,
    /// Cargo's own total for the build, which includes everything outside the unit model.
    pub reported_total: Option<Millis>,
    /// The targets the build was asked for, as one display string.
    pub targets: Option<String>,
    /// When the build started, as an RFC 3339 timestamp, exactly as the report wrote it.
    pub build_start: Option<String>,
}

impl TimingReport {
    /// The last unit's end minus the first unit's start: the part of the build that compiled things.
    ///
    /// Predictions are compared against this rather than against Cargo's reported total, because a
    /// registry update or a crate download sits on the same clock and is not work any graph
    /// transformation can shorten.
    #[must_use]
    pub fn compilation_span(&self) -> Millis {
        let first = self.units.iter().map(|u| u.start).min();
        let last = self
            .units
            .iter()
            .map(|u| u.start.saturating_add(u.duration))
            .max();
        match (first, last) {
            (Some(first), Some(last)) => last.saturating_sub(first),
            _ => Millis::ZERO,
        }
    }

    /// Cargo's reported total minus the compilation span: fetching, resolution, and anything else
    /// outside the unit model.
    ///
    /// Reported separately rather than folded in. A build that spent 28 s downloading and 4.7 s
    /// compiling is not a 32.5 s compilation problem.
    #[must_use]
    pub fn excluded_prefix(&self) -> Millis {
        match self.reported_total {
            Some(total) => total.saturating_sub(self.compilation_span()),
            None => Millis::ZERO,
        }
    }
}

/// Reads a `cargo-timing.html` report.
///
/// # Errors
///
/// [`ParseError::NoUnitData`] or [`ParseError::TruncatedArray`] if the data islands are absent or
/// incomplete, [`ParseError::BadUnitField`] if an entry has a shape this parser does not know, and
/// [`ParseError::FreshUnits`] if the build reused cached units, whose recorded durations are zero.
pub fn parse(text: &str) -> Result<TimingReport, ParseError> {
    crate::error::reject_wide_text(text, "timing report")?;
    // The unit array is located first, so that a file which is not a timing report at all is named
    // as such rather than as a file with a missing table row.
    let raw = array_literal(text, "UNIT_DATA")?.ok_or(ParseError::NoUnitData)?;

    let summary = summary_table(text)?;
    let fresh_units = summary.number("Fresh units")?;
    let dirty_units = summary.number("Dirty units")?;
    let total_units = summary
        .number("Total units")
        .unwrap_or_else(|_| fresh_units.saturating_add(dirty_units));
    if fresh_units > 0 {
        return Err(ParseError::FreshUnits {
            fresh: fresh_units,
            total: total_units,
        });
    }

    let value: Value = serde_json::from_str(&raw).map_err(|e| ParseError::MalformedArray {
        name: "UNIT_DATA".to_owned(),
        detail: e.to_string(),
    })?;
    let entries = value.as_array().ok_or_else(|| ParseError::MalformedArray {
        name: "UNIT_DATA".to_owned(),
        detail: "the literal is not an array".to_owned(),
    })?;

    let mut units = Vec::with_capacity(entries.len());
    for (position, entry) in entries.iter().enumerate() {
        units.push(unit(entry, position)?);
    }

    let (max_concurrency, jobs, cpus) = concurrency_row(&summary);
    let (rustc_version, host, target) = compiler_row(&summary);

    Ok(TimingReport {
        units,
        fresh_units,
        dirty_units,
        total_units,
        jobs,
        cpus,
        max_concurrency,
        profile: summary.text("Profile"),
        rustc_version,
        host,
        target,
        reported_total: summary.text("Total time").and_then(|v| seconds_prefix(&v)),
        targets: summary.text("Targets"),
        build_start: summary.text("Build start"),
    })
}

fn unit(entry: &Value, position: usize) -> Result<TimingUnit, ParseError> {
    let bad = |field: &'static str, expected: &'static str| ParseError::BadUnitField {
        entry: position,
        field,
        expected,
    };

    let index = entry
        .get("i")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad("i", "a unit index"))? as usize;
    let package = entry
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("name", "a package name"))?
        .to_owned();
    let version = entry
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let display = entry
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("target", "a target display string"))?;
    let start_seconds = entry
        .get("start")
        .and_then(Value::as_f64)
        .ok_or_else(|| bad("start", "a number of seconds"))?;
    let duration_seconds = entry
        .get("duration")
        .and_then(Value::as_f64)
        .ok_or_else(|| bad("duration", "a number of seconds"))?;

    let start = millis_from_seconds(start_seconds, position)?;
    let duration = millis_from_seconds(duration_seconds, position)?;

    let mut features: Vec<String> = entry
        .get("features")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    features.sort();
    features.dedup();

    // `sections` is `[[name, {start, end}], …]` with times relative to the unit's own start. On
    // stable it is present exactly for pipelined units.
    let frontend_end = entry
        .get("sections")
        .and_then(Value::as_array)
        .and_then(|sections| {
            sections.iter().find_map(|section| {
                let pair = section.as_array()?;
                if pair.first().and_then(Value::as_str)? != "frontend" {
                    return None;
                }
                pair.get(1)?.get("end")?.as_f64()
            })
        });
    let metadata_at = match frontend_end {
        Some(seconds) => Some(millis_from_seconds(seconds, position)?.max(Millis::ZERO)),
        None => None,
    };
    // Metadata cannot be emitted after the unit that emits it has ended, and the core rejects a
    // graph that claims otherwise, so a section that overruns is clamped here where the reason is
    // visible rather than turned into an error the user cannot act on.
    let metadata_at = metadata_at.map(|at| if at > duration { duration } else { at });

    Ok(TimingUnit {
        index,
        position,
        package,
        version,
        target: target_of(display),
        features,
        start,
        duration,
        metadata_at,
        unblocked: indices(entry.get("unblocked_units")),
        unblocked_rmeta: indices(entry.get("unblocked_rmeta_units")),
    })
}

fn indices(value: Option<&Value>) -> Vec<usize> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_u64)
                .map(|i| i as usize)
                .collect()
        })
        .unwrap_or_default()
}

/// The one place a report's floating-point seconds become integer milliseconds.
///
/// Cargo writes two decimal places, so rounding to the nearest millisecond loses nothing that was
/// ever measured. A value that is not a finite, non-negative number of seconds small enough to be
/// counted in milliseconds is refused rather than saturated, because a saturated weight would look
/// like a real measurement.
fn millis_from_seconds(seconds: f64, entry: usize) -> Result<Millis, ParseError> {
    const LIMIT: f64 = 1e15;
    if !seconds.is_finite() || !(0.0..=LIMIT).contains(&seconds) {
        return Err(ParseError::DurationOutOfRange { entry, seconds });
    }
    Ok(Millis::new((seconds * 1000.0).round() as u64))
}

fn target_of(display: &str) -> TimingTarget {
    // The observed alphabet, note the leading space on everything but a library.
    match display {
        "" => return TimingTarget::Library,
        " build-script" => return TimingTarget::BuildScriptCompile,
        " build-script (run)" => return TimingTarget::BuildScriptRun,
        _ => {}
    }
    // ` name "kind"`
    let trimmed = display.trim();
    if let Some((name, rest)) = trimmed.split_once(' ') {
        let kind = rest.trim().trim_matches('"');
        if !name.is_empty() && !kind.is_empty() && !kind.contains(char::is_whitespace) {
            return TimingTarget::Named {
                name: name.to_owned(),
                kind: kind.to_owned(),
            };
        }
    }
    TimingTarget::Other(trimmed.to_owned())
}

/// `12 (jobs=10 ncpu=10)` split into its three numbers.
fn concurrency_row(summary: &Summary) -> (Option<u32>, Option<u32>, Option<u32>) {
    let Some(value) = summary.text("Max concurrency") else {
        return (None, None, None);
    };
    let peak = value.split_whitespace().next().and_then(|n| n.parse().ok());
    let field = |name: &str| {
        value
            .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
            .find_map(|part| part.strip_prefix(name)?.parse().ok())
    };
    (peak, field("jobs="), field("ncpu="))
}

/// `rustc 1.95.0 (…)<br>Host: …<br>Target: …` split into its three parts.
fn compiler_row(summary: &Summary) -> (Option<String>, Option<String>, Option<String>) {
    let Some(raw) = summary.raw("rustc") else {
        return (None, None, None);
    };
    let mut version = None;
    let mut host = None;
    let mut target = None;
    for part in raw.split("<br>") {
        let text = strip_tags(part);
        if let Some(rest) = text.strip_prefix("Host:") {
            host = Some(rest.trim().to_owned());
        } else if let Some(rest) = text.strip_prefix("Target:") {
            target = Some(rest.trim().to_owned());
        } else if !text.is_empty() && version.is_none() {
            version = Some(text);
        }
    }
    (version, host, target)
}

/// The leading `61.2s` of `61.2s (1m 1.2s)`.
fn seconds_prefix(value: &str) -> Option<Millis> {
    let digits: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let seconds: f64 = digits.parse().ok()?;
    millis_from_seconds(seconds, 0).ok()
}

/// The report's first table, as label-and-value pairs in the order they appeared.
struct Summary {
    rows: Vec<(String, String)>,
}

impl Summary {
    fn raw(&self, label: &str) -> Option<&str> {
        self.rows
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, value)| value.as_str())
    }

    fn text(&self, label: &str) -> Option<String> {
        self.raw(label).map(strip_tags)
    }

    fn number(&self, label: &'static str) -> Result<usize, ParseError> {
        let value = self
            .text(label)
            .ok_or(ParseError::MissingSummaryRow { row: label })?;
        value.trim().parse().map_err(|_| ParseError::BadSummaryRow {
            row: label,
            value,
            expected: "a count",
        })
    }
}

fn summary_table(text: &str) -> Result<Summary, ParseError> {
    let start = text.find("<table").ok_or(ParseError::NoSummaryTable)?;
    let rest = text.get(start..).ok_or(ParseError::NoSummaryTable)?;
    let end = rest.find("</table>").ok_or(ParseError::NoSummaryTable)?;
    let table = rest.get(..end).ok_or(ParseError::NoSummaryTable)?;

    // The summary table is a flat list of alternating label and value cells, so the cells are
    // collected and paired. Matching `<tr>` blocks would buy nothing and would break on the
    // whitespace Cargo puts between them.
    let mut cells = Vec::new();
    let mut remaining = table;
    while let Some(open) = remaining.find("<td>") {
        let after = match remaining.get(open.saturating_add(4)..) {
            Some(after) => after,
            None => break,
        };
        let Some(close) = after.find("</td>") else {
            break;
        };
        if let Some(cell) = after.get(..close) {
            cells.push(cell);
        }
        remaining = match after.get(close.saturating_add(5)..) {
            Some(rest) => rest,
            None => break,
        };
    }

    let mut rows = Vec::new();
    let mut pairs = cells.chunks_exact(2);
    for pair in &mut pairs {
        let (Some(label), Some(value)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let label = strip_tags(label);
        let label = label.trim().trim_end_matches(':').to_owned();
        rows.push((label, (*value).to_owned()));
    }
    if rows.is_empty() {
        return Err(ParseError::NoSummaryTable);
    }
    Ok(Summary { rows })
}

/// A cell's text: markup removed, entities decoded, whitespace collapsed at the ends.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut depth = 0usize;
    for c in html.chars() {
        match c {
            '<' => depth = depth.saturating_add(1),
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    decode_entities(out.trim())
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        if let Some(head) = rest.get(..at) {
            out.push_str(head);
        }
        let tail = rest.get(at..).unwrap_or("");
        let replacement = [
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&#39;", "'"),
        ]
        .into_iter()
        .find(|(entity, _)| tail.starts_with(entity));
        match replacement {
            Some((entity, decoded)) => {
                out.push_str(decoded);
                rest = tail.get(entity.len()..).unwrap_or("");
            }
            None => {
                out.push('&');
                rest = tail.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}

/// The array literal assigned to `const <name> = […];`, with brackets matched so that a truncated
/// file is distinguishable from a malformed one.
fn array_literal(text: &str, name: &str) -> Result<Option<String>, ParseError> {
    let needle = format!("const {name} = ");
    let Some(at) = text.find(&needle) else {
        return Ok(None);
    };
    let rest = text
        .get(at.saturating_add(needle.len())..)
        .unwrap_or_default();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, c) in rest.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '[' | '{' => depth = depth.saturating_add(1),
            ']' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = offset.saturating_add(c.len_utf8());
                    return Ok(rest.get(..end).map(str::to_owned));
                }
            }
            _ => {}
        }
    }
    Err(ParseError::TruncatedArray {
        name: name.to_owned(),
    })
}
