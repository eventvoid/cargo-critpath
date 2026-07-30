//! Everything that can go wrong reading a build's output.

use core::fmt;

use critpath_core::GraphError;

/// Enough of a unit's identity to name it in an error message.
///
/// A unit that could not be matched has to be described from whichever side saw it, so the fields
/// that side does not know are empty rather than invented.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct UnitDescription {
    /// Package name, where it is known.
    pub package: String,
    /// Package version, where it is known.
    pub version: String,
    /// Target within the package: `lib`, a binary name, or a build-script phase.
    pub target: String,
    /// Where in its input the unit was seen, counted from one, for locating it by eye.
    pub position: usize,
}

impl fmt::Display for UnitDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let package = if self.package.is_empty() {
            "<unknown package>"
        } else {
            &self.package
        };
        write!(f, "{package}")?;
        if !self.version.is_empty() {
            write!(f, " {}", self.version)?;
        }
        if !self.target.is_empty() {
            write!(f, " ({})", self.target)?;
        }
        write!(f, " [#{}]", self.position)
    }
}

/// A build's output that could not be turned into a graph, and why.
///
/// Every variant's message states what to do next, because the usual cause is that the build the
/// output came from was not the kind of build the analysis needs.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ParseError {
    /// The input holds NUL characters, so it is not the UTF-8 text Cargo wrote.
    NotUtf8 {
        /// What was being read, for the message: `build log` or `timing report`.
        what: &'static str,
        /// How many NUL characters were found.
        nuls: usize,
    },
    /// The verbose log contained no compiler invocations at all.
    NoInvocations,
    /// A `Running` line could not be split into arguments because a quote was left open.
    UnbalancedQuoting {
        /// The offending line, truncated.
        line: String,
    },
    /// The timing report contained no `UNIT_DATA` array.
    NoUnitData,
    /// A JavaScript array literal in the timing report was not valid JSON.
    MalformedArray {
        /// The name of the assignment, such as `UNIT_DATA`.
        name: String,
        /// What `serde_json` said about it.
        detail: String,
    },
    /// An array literal in the timing report was cut off before its closing bracket.
    TruncatedArray {
        /// The name of the assignment, such as `UNIT_DATA`.
        name: String,
    },
    /// A `UNIT_DATA` entry was missing a field, or the field had the wrong type.
    BadUnitField {
        /// Which entry, counted from zero as the report numbers them.
        entry: usize,
        /// The field that was wrong.
        field: &'static str,
        /// What was expected of it.
        expected: &'static str,
    },
    /// The timing report's summary table was absent or unreadable.
    NoSummaryTable,
    /// The timing report's summary table lacked a row the analysis needs.
    MissingSummaryRow {
        /// The row's label, as Cargo writes it.
        row: &'static str,
    },
    /// A summary row was present but did not hold the expected kind of value.
    BadSummaryRow {
        /// The row's label, as Cargo writes it.
        row: &'static str,
        /// What the row actually contained.
        value: String,
        /// What was expected of it.
        expected: &'static str,
    },
    /// The build reused cached units, so most of its weights are zero.
    FreshUnits {
        /// How many units were reused.
        fresh: usize,
        /// How many units the build had in total.
        total: usize,
    },
    /// The build compiled nothing.
    EmptyBuild,
    /// A duration in the timing report is not a duration.
    DurationOutOfRange {
        /// Which entry, counted from zero.
        entry: usize,
        /// The value that could not be used.
        seconds: f64,
    },
    /// The two inputs describe different sets of units.
    UnmatchedUnits {
        /// Units the verbose log has and the timing report does not.
        in_log: Vec<UnitDescription>,
        /// Units the timing report has and the verbose log does not.
        in_report: Vec<UnitDescription>,
        /// Whether `cargo metadata` output was supplied.
        had_metadata: bool,
    },
    /// A `--extern` names an artifact no invocation in this log produced.
    UnresolvedDependency {
        /// The unit that named it, boxed so that a failure path does not widen every `Result` in
        /// the crate.
        consumer: Box<UnitDescription>,
        /// The `--extern` name.
        dependency: String,
        /// The artifact path's producer hash, where there was one.
        producer: String,
    },
    /// `cargo metadata` output could not be read.
    Metadata {
        /// What was wrong with it.
        detail: String,
    },
    /// The assembled graph was rejected by the core.
    Graph(GraphError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 { what, nuls } => write!(
                f,
                "the {what} holds {nuls} NUL characters, so it is not the UTF-8 text Cargo wrote; \
                 a redirection under PowerShell writes UTF-16. Capture it with \
                 `cargo build --timings -v --offline 2>&1 | Out-File -Encoding utf8 build.log`, \
                 or from a shell that redirects as UTF-8"
            ),
            Self::NoInvocations => write!(
                f,
                "the build log contains no compiler invocations; \
                 it was captured without -v, or every unit was already up to date. \
                 Run `cargo clean` and then \
                 `cargo build --timings -v --offline 2>build.log`"
            ),
            Self::UnbalancedQuoting { line } => write!(
                f,
                "a `Running` line in the build log has an unclosed quote and cannot be split into \
                 arguments: {line}. \
                 The log is probably truncated; capture it again, redirecting stderr to a file \
                 rather than copying it out of a terminal"
            ),
            Self::NoUnitData => write!(
                f,
                "the timing report contains no `UNIT_DATA` array, so it holds no per-unit times. \
                 Check that the file is the `cargo-timing.html` Cargo wrote and not the \
                 `cargo-timing-<date>.html` copy of an older run, and that the build used --timings"
            ),
            Self::MalformedArray { name, detail } => write!(
                f,
                "the `{name}` array in the timing report is not valid JSON ({detail}); \
                 this Cargo version writes a shape the parser does not know. \
                 Report the Cargo version, and in the meantime the analysis cannot use this report"
            ),
            Self::TruncatedArray { name } => write!(
                f,
                "the `{name}` array in the timing report is cut off before its closing bracket, \
                 so the file is incomplete; \
                 the build was probably interrupted, so run it again to completion"
            ),
            Self::BadUnitField {
                entry,
                field,
                expected,
            } => write!(
                f,
                "entry {entry} of `UNIT_DATA` has no usable `{field}` (expected {expected}); \
                 this Cargo version writes a shape the parser does not know, \
                 so report the Cargo version"
            ),
            Self::NoSummaryTable => write!(
                f,
                "the timing report has no summary table, so the build's unit counts and job limit \
                 cannot be read; \
                 check that the file is a complete `cargo-timing.html`"
            ),
            Self::MissingSummaryRow { row } => write!(
                f,
                "the timing report's summary table has no `{row}` row, \
                 so the analysis cannot tell whether the build was a clean one; \
                 this Cargo version writes a shape the parser does not know, \
                 so report the Cargo version"
            ),
            Self::BadSummaryRow {
                row,
                value,
                expected,
            } => write!(
                f,
                "the timing report's `{row}` row holds {value:?}, which is not {expected}; \
                 this Cargo version writes a shape the parser does not know, \
                 so report the Cargo version"
            ),
            Self::FreshUnits { fresh, total } => write!(
                f,
                "{fresh} of this build's {total} units were reused from the cache, so their \
                 recorded durations are zero and any critical path computed from them would be \
                 meaningless. \
                 Run `cargo clean` first, then \
                 `cargo build --timings -v --offline 2>build.log`"
            ),
            Self::EmptyBuild => write!(
                f,
                "the build compiled nothing, so there is no graph to analyse; \
                 run `cargo clean` and build again"
            ),
            Self::DurationOutOfRange { entry, seconds } => write!(
                f,
                "entry {entry} of `UNIT_DATA` reports a duration of {seconds} seconds, which is \
                 not a duration this analysis can use; \
                 the report is corrupt, so run the build again"
            ),
            Self::UnmatchedUnits {
                in_log,
                in_report,
                had_metadata,
            } => {
                write!(
                    f,
                    "the build log and the timing report describe different sets of units, \
                     so a graph built from them would silently omit work"
                )?;
                if !in_log.is_empty() {
                    write!(f, "; only in the build log: ")?;
                    write_list(f, in_log)?;
                }
                if !in_report.is_empty() {
                    write!(f, "; only in the timing report: ")?;
                    write_list(f, in_report)?;
                }
                write!(
                    f,
                    ". Both must come from the same build, so capture them together with "
                )?;
                write!(f, "`cargo build --timings -v --offline 2>build.log`")?;
                if *had_metadata {
                    write!(
                        f,
                        ". The `cargo metadata` output supplied did not cover these units, so \
                         check that it came from the same workspace"
                    )
                } else {
                    write!(
                        f,
                        ". Supplying `cargo metadata --format-version 1` output as well would let \
                         a workspace-local package be attributed to its manifest exactly instead \
                         of by its directory name"
                    )
                }
            }
            Self::UnresolvedDependency {
                consumer,
                dependency,
                producer,
            } => write!(
                f,
                "unit {consumer} depends on `{dependency}` through artifact {producer}, which no \
                 invocation in this log produced, so an edge would be missing from the graph and \
                 the critical path would be understated. \
                 That happens when part of the build was already up to date: \
                 run `cargo clean` and build again"
            ),
            Self::Metadata { detail } => write!(
                f,
                "the `cargo metadata` output could not be read ({detail}); \
                 produce it with `cargo metadata --format-version 1`, \
                 or omit it and local packages will be attributed by directory name instead"
            ),
            Self::Graph(inner) => write!(
                f,
                "the graph assembled from this build was rejected: {inner}. \
                 This is a defect in the adapter rather than in the build, so please report it \
                 with the Cargo version"
            ),
        }
    }
}

fn write_list(f: &mut fmt::Formatter<'_>, units: &[UnitDescription]) -> fmt::Result {
    const SHOWN: usize = 8;
    for (i, unit) in units.iter().take(SHOWN).enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{unit}")?;
    }
    if units.len() > SHOWN {
        write!(f, ", and {} more", units.len().saturating_sub(SHOWN))?;
    }
    Ok(())
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Graph(inner) => Some(inner),
            _ => None,
        }
    }
}

impl From<GraphError> for ParseError {
    fn from(inner: GraphError) -> Self {
        Self::Graph(inner)
    }
}

/// Refuses text that holds NUL characters.
///
/// Cargo's output is UTF-8 and never contains a NUL. UTF-16LE without a byte-order mark decodes as
/// valid UTF-8 in which every second character is one, so this is what tells a mis-encoded capture
/// apart from an empty one. A stray NUL is not tolerated either: there is no reading under which
/// Cargo would have written one.
pub(crate) fn reject_wide_text(text: &str, what: &'static str) -> Result<(), ParseError> {
    let nuls = text.bytes().filter(|b| *b == 0).count();
    if nuls > 0 {
        return Err(ParseError::NotUtf8 { what, nuls });
    }
    Ok(())
}
