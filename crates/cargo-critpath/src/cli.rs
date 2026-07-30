//! The command line: arguments, the pipeline they drive, and the exit codes.
//!
//! Cargo invokes an external subcommand as `cargo-critpath critpath <args>`, so a leading
//! `critpath` word is accepted and skipped. Everything is written to a caller-supplied stream
//! rather than to the process's own, which is what lets the tests read the output back.

use core::fmt;
use std::ffi::OsString;
use std::io::{self, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use critpath_core::{analyze, Millis, Policy};
use lexopt::ValueExt;

use crate::findings;
use crate::timings::TimingReport;
use crate::{json, parse_with_metadata, report, ParseError};

/// What the tool did, and the exit code that says so.
///
/// The codes are a contract with CI scripts: a threshold that was exceeded has to be
/// distinguishable from a tool that could not run, or the script has to parse prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The analysis ran and every threshold was met.
    Completed,
    /// The arguments were wrong, or a path could not be read.
    Usage,
    /// The input was not something this tool can analyse.
    Rejected,
    /// The analysis ran and a `--fail-if` threshold was exceeded.
    ThresholdExceeded,
}

impl Outcome {
    /// The process exit code for this outcome.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Usage => 1,
            Self::Rejected => 2,
            Self::ThresholdExceeded => 3,
        }
    }
}

/// The help text, written by hand because the tool has no argument-parsing framework to generate
/// one. See `docs/decisions.md`, D3, for why.
const HELP: &str = "\
cargo-critpath — what to change in a Cargo build, and how much time it would save

USAGE
    cargo critpath --log <path> --timings <path> [options]

    Capture the two inputs from one clean build:

        cargo clean
        cargo build --timings -v --offline 2>build.log
        cargo critpath --log build.log --timings target/cargo-timings/cargo-timing.html

REQUIRED
    --log <path>        the verbose build log: Cargo's stderr from `cargo build -v`
    --timings <path>    a `cargo-timing.html` report from the same build. Repeatable: give one
                        per clean build of the same commit and the run-to-run spread of the
                        critical path is measured, so that a saving smaller than the noise can
                        be marked as such

OPTIONS
    --metadata <path>   `cargo metadata --format-version 1` output, which attributes a
                        workspace-local source path to its package exactly rather than by
                        directory name
    --jobs <n>          how many executors to analyse for. Defaults to the -j the build ran with
    --format <f>        human (default) or json. The JSON contract is documented at
                        https://github.com/eventvoid/cargo-critpath/blob/main/docs/json-schema.md
    --policy <p>        cargo-like (default), which reproduces Cargo's cost-blind ordering, or
                        longest-path, which schedules better than Cargo does. The gap between
                        them is what Cargo's ordering alone costs
    --include-units     add the per-unit array to the JSON output
    --top <n>           how many ranked units and scenarios to show (default 10)
    --fail-if-critical-path-over <ms>
                        exit 3 if the critical path is longer than this
    --help              print this and exit
    --version           print the version and exit

EXIT CODES
    0  the analysis completed
    1  the arguments were wrong, or a path could not be read
    2  the input was rejected: fresh units, an unreadable report, or a build that was not
       `cargo build`
    3  the analysis completed and a --fail-if threshold was exceeded

WHAT THIS DOES NOT DO
    It analyses `cargo build` only, and refuses a report from `cargo check`, `cargo test` or
    `cargo doc` by name: those are different graphs, not variations on one.
    Every weight comes from one real build at one executor count and is not invariant to it, so
    an absolute time for another executor count is marked unreliable rather than presented.
    A split cannot be modelled exactly, so it is reported as an interval with its assumptions
    printed, never as a single number.
";

/// The options a run was asked for.
struct Options {
    log: PathBuf,
    timings: Vec<PathBuf>,
    metadata: Option<PathBuf>,
    jobs: Option<NonZeroU32>,
    format: Format,
    policy: Policy,
    include_units: bool,
    top: usize,
    fail_if_over: Option<Millis>,
}

/// Which renderer to use.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Human,
    Json,
}

/// What the arguments asked the tool to do.
enum Command {
    Help,
    Version,
    Analyse(Box<Options>),
}

/// Every option the tool accepts, for the message an unknown one produces.
const OPTIONS: &str = "--log, --timings, --metadata, --jobs, --format, --policy, --include-units, \
                       --top, --fail-if-critical-path-over, --help, --version";

/// An argument that could not be used, and what to do about it.
enum UsageError {
    Unknown(String),
    Unexpected(String),
    MissingValue(String),
    BadValue {
        option: &'static str,
        value: String,
        expected: &'static str,
    },
    Missing(&'static str),
    Unreadable {
        path: String,
        detail: String,
    },
    /// The file was read but holds something other than the UTF-8 text Cargo writes.
    MisEncoded {
        detail: String,
    },
    ExecutorCountUnknown,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(option) => write!(
                f,
                "unknown option `{option}`. The options are: {OPTIONS}. \
                 Run `cargo critpath --help` for what each one does"
            ),
            Self::Unexpected(value) => write!(
                f,
                "unexpected argument `{value}`: this tool takes options only, apart from the \
                 `critpath` word Cargo itself inserts. The options are: {OPTIONS}"
            ),
            Self::MissingValue(option) => write!(
                f,
                "the option `{option}` needs a value and was given none. \
                 Run `cargo critpath --help` for what it expects"
            ),
            Self::BadValue {
                option,
                value,
                expected,
            } => write!(
                f,
                "`{value}` is not a usable value for `{option}`: {expected}"
            ),
            Self::Missing(option) => write!(
                f,
                "`{option}` is required and was not given. \
                 Capture both inputs from one clean build with \
                 `cargo build --timings -v --offline 2>build.log`, then pass the log to `--log` \
                 and `target/cargo-timings/cargo-timing.html` to `--timings`"
            ),
            Self::Unreadable { path, detail } => write!(
                f,
                "cannot read `{path}`: {detail}. \
                 Check the path, and that the build that was supposed to write it completed"
            ),
            // No advice about the path or the build: the file was found and read, and neither is
            // the problem.
            Self::MisEncoded { detail } => write!(f, "{detail}"),
            Self::ExecutorCountUnknown => write!(
                f,
                "the timing report does not record what `-j` the build ran with, so there is no \
                 executor count to analyse for. Pass `--jobs <n>` to choose one"
            ),
        }
    }
}

impl From<lexopt::Error> for UsageError {
    fn from(error: lexopt::Error) -> Self {
        match error {
            lexopt::Error::UnexpectedOption(option) => Self::Unknown(option),
            lexopt::Error::UnexpectedArgument(value) => {
                Self::Unexpected(value.to_string_lossy().into_owned())
            }
            lexopt::Error::MissingValue { option } => {
                Self::MissingValue(option.unwrap_or_else(|| "an option".to_owned()))
            }
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// Runs the tool, writing its report to `out` and anything that went wrong to `err`.
///
/// `args` is the whole command line including the program name, as [`std::env::args_os`] gives it.
/// Nothing is written to the process's own streams, so a caller can capture both.
///
/// ```no_run
/// let mut out = Vec::new();
/// let mut err = Vec::new();
/// let outcome = cargo_critpath::cli::run(std::env::args_os(), &mut out, &mut err);
/// std::process::exit(outcome.code());
/// ```
pub fn run<I>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> Outcome
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let command = match parse(args) {
        Ok(command) => command,
        Err(problem) => {
            let _ = writeln!(err, "error: {problem}");
            return Outcome::Usage;
        }
    };
    match command {
        Command::Help => {
            let _ = write!(out, "{HELP}");
            Outcome::Completed
        }
        Command::Version => {
            let _ = writeln!(out, "cargo-critpath {}", env!("CARGO_PKG_VERSION"));
            Outcome::Completed
        }
        Command::Analyse(options) => analyse(&options, out, err),
    }
}

fn parse<I>(args: I) -> Result<Command, UsageError>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let mut log: Option<PathBuf> = None;
    let mut timings: Vec<PathBuf> = Vec::new();
    let mut metadata: Option<PathBuf> = None;
    let mut jobs: Option<NonZeroU32> = None;
    let mut format = Format::Human;
    let mut policy = Policy::CargoLike;
    let mut include_units = false;
    let mut top = 10usize;
    let mut fail_if_over: Option<Millis> = None;
    let mut seen_subcommand = false;

    let mut parser = lexopt::Parser::from_iter(args);
    while let Some(argument) = parser.next()? {
        match argument {
            lexopt::Arg::Long("log") => log = Some(PathBuf::from(parser.value()?)),
            lexopt::Arg::Long("timings") => timings.push(PathBuf::from(parser.value()?)),
            lexopt::Arg::Long("metadata") => metadata = Some(PathBuf::from(parser.value()?)),
            lexopt::Arg::Long("jobs") => {
                let raw = parser.value()?.string()?;
                jobs = Some(raw.parse().ok().and_then(NonZeroU32::new).ok_or(
                    UsageError::BadValue {
                        option: "--jobs",
                        value: raw.clone(),
                        expected: "it must be a whole number of executors, at least 1",
                    },
                )?);
            }
            lexopt::Arg::Long("format") => {
                let raw = parser.value()?.string()?;
                format = match raw.as_str() {
                    "human" => Format::Human,
                    "json" => Format::Json,
                    _ => {
                        return Err(UsageError::BadValue {
                            option: "--format",
                            value: raw,
                            expected: "the formats are `human` and `json`",
                        })
                    }
                };
            }
            lexopt::Arg::Long("policy") => {
                let raw = parser.value()?.string()?;
                policy = match raw.as_str() {
                    "cargo-like" => Policy::CargoLike,
                    "longest-path" => Policy::LongestPath,
                    _ => {
                        return Err(UsageError::BadValue {
                            option: "--policy",
                            value: raw,
                            expected: "the policies are `cargo-like` and `longest-path`",
                        })
                    }
                };
            }
            lexopt::Arg::Long("include-units") => include_units = true,
            lexopt::Arg::Long("top") => {
                let raw = parser.value()?.string()?;
                top = raw.parse().map_err(|_| UsageError::BadValue {
                    option: "--top",
                    value: raw.clone(),
                    expected: "it must be a whole number of entries, and may be 0",
                })?;
            }
            lexopt::Arg::Long("fail-if-critical-path-over") => {
                let raw = parser.value()?.string()?;
                let ms: u64 = raw.parse().map_err(|_| UsageError::BadValue {
                    option: "--fail-if-critical-path-over",
                    value: raw.clone(),
                    expected: "it must be a whole number of milliseconds",
                })?;
                fail_if_over = Some(Millis::new(ms));
            }
            lexopt::Arg::Long("help") => return Ok(Command::Help),
            lexopt::Arg::Long("version") => return Ok(Command::Version),
            // Cargo runs an external subcommand as `cargo-critpath critpath …`, so the first
            // positional argument is the subcommand's own name.
            lexopt::Arg::Value(value) if !seen_subcommand && value == *"critpath" => {
                seen_subcommand = true;
            }
            lexopt::Arg::Value(value) => {
                return Err(UsageError::Unexpected(value.to_string_lossy().into_owned()))
            }
            lexopt::Arg::Long(name) => return Err(UsageError::Unknown(format!("--{name}"))),
            lexopt::Arg::Short(letter) => return Err(UsageError::Unknown(format!("-{letter}"))),
        }
    }

    let log = log.ok_or(UsageError::Missing("--log"))?;
    if timings.is_empty() {
        return Err(UsageError::Missing("--timings"));
    }
    Ok(Command::Analyse(Box::new(Options {
        log,
        timings,
        metadata,
        jobs,
        format,
        policy,
        include_units,
        top,
        fail_if_over,
    })))
}

fn analyse(options: &Options, out: &mut dyn Write, err: &mut dyn Write) -> Outcome {
    let log = match read(&options.log) {
        Ok(text) => text,
        Err(problem) => return usage(err, &problem),
    };
    let metadata = match options.metadata.as_deref() {
        Some(path) => match read(path) {
            Ok(text) => Some(text),
            Err(problem) => return usage(err, &problem),
        },
        None => None,
    };
    let mut reports = Vec::with_capacity(options.timings.len());
    for path in &options.timings {
        match read(path) {
            Ok(text) => reports.push(text),
            Err(problem) => return usage(err, &problem),
        }
    }

    // The subcommand is decided before anything is matched, because a parser written against the
    // `cargo build` alphabet does not merely degrade on another subcommand's report: it silently
    // produces a smaller graph with a meaningless critical path.
    for text in &reports {
        let report = match crate::parse_timing_report(text) {
            Ok(report) => report,
            Err(problem) => return rejected(err, &problem),
        };
        if let Some(subcommand) = subcommand_of(&report) {
            let _ = writeln!(
                err,
                "error: this timing report is from `{subcommand}`, which this tool does not \
                 analyse. Only `cargo build` is supported, because the other subcommands build \
                 genuinely different graphs: on one measured project, `cargo build` and \
                 `cargo check` shared exactly one unit on their critical paths. Capture a build \
                 with `cargo clean && cargo build --timings -v --offline 2>build.log`"
            );
            return Outcome::Rejected;
        }
    }

    let mut builds = Vec::with_capacity(reports.len());
    for text in &reports {
        match parse_with_metadata(&log, text, metadata.as_deref()) {
            Ok(build) => builds.push(build),
            Err(problem) => return rejected(err, &problem),
        }
    }
    let Some(primary) = builds.first() else {
        return usage(err, &UsageError::Missing("--timings"));
    };

    let executors = match options
        .jobs
        .or_else(|| primary.build.jobs.and_then(NonZeroU32::new))
    {
        Some(executors) => executors,
        None => return usage(err, &UsageError::ExecutorCountUnknown),
    };
    let samples: Vec<Millis> = builds
        .iter()
        .map(|build| analyze(&build.graph).critical_path_len())
        .collect();

    let findings = findings::assemble(primary, &samples, executors, options.policy, options.top);
    let written = match options.format {
        Format::Human => report::write(out, &findings),
        Format::Json => json::write(out, &findings, options.include_units),
    };
    if let Err(problem) = written {
        let _ = writeln!(err, "error: cannot write the report: {problem}");
        return Outcome::Usage;
    }

    match options.fail_if_over {
        Some(limit) if findings.baseline.critical_path > limit => {
            let _ = writeln!(
                err,
                "error: the critical path is {} against the {limit} limit given by \
                 --fail-if-critical-path-over",
                findings.baseline.critical_path
            );
            Outcome::ThresholdExceeded
        }
        _ => Outcome::Completed,
    }
}

/// The Cargo subcommand a timing report came from, when it was not `cargo build`.
///
/// The `target` field is a display string whose alphabet depends on the subcommand, recorded in
/// `docs/cargo-data-sources.md`: `cargo check` suffixes `(check)`, `cargo test` `(test)` and
/// `(doc test)`, and so on. `cargo build` never suffixes anything.
fn subcommand_of(report: &TimingReport) -> Option<&'static str> {
    const MARKERS: [(&str, &str); 5] = [
        ("(doc test)", "cargo test"),
        ("(test)", "cargo test"),
        ("(bench)", "cargo bench"),
        ("(check)", "cargo check"),
        ("(doc)", "cargo doc"),
    ];
    report.units.iter().find_map(|unit| {
        let label = unit.target.label();
        MARKERS
            .iter()
            .find(|(marker, _)| label.ends_with(marker))
            .map(|(_, command)| *command)
    })
}

fn read(path: &Path) -> Result<String, UsageError> {
    crate::collect::read_text(path).map_err(|problem| {
        if problem.kind() == io::ErrorKind::InvalidData {
            UsageError::MisEncoded {
                detail: problem.to_string(),
            }
        } else {
            UsageError::Unreadable {
                path: path.display().to_string(),
                detail: problem.to_string(),
            }
        }
    })
}

fn usage(err: &mut dyn Write, problem: &UsageError) -> Outcome {
    let _ = writeln!(err, "error: {problem}");
    match problem {
        // The file was found and read; what it holds is unusable. That is the same outcome as a
        // report this tool cannot analyse, and it must not depend on whether the file happened to
        // carry a byte-order mark: a mis-encoded log without one is caught by the parser and
        // reported as rejected, and one with a BOM has to agree.
        UsageError::MisEncoded { .. } => Outcome::Rejected,
        _ => Outcome::Usage,
    }
}

fn rejected(err: &mut dyn Write, problem: &ParseError) -> Outcome {
    let _ = writeln!(err, "error: {problem}");
    Outcome::Rejected
}
