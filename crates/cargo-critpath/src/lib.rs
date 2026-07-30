//! Turn the output of a real Cargo build into a [`critpath_core::BuildGraph`].
//!
//! Two inputs are needed, and one `cargo build --timings -v --offline` produces both:
//!
//! - the **verbose build log**, which contains one `rustc` invocation per dirty unit and therefore
//!   states the unit set, the edge set and each edge's kind exactly;
//! - the **timing report**, `target/cargo-timings/cargo-timing.html`, which contains each unit's
//!   wall duration and the moment it emitted its metadata.
//!
//! Neither input is sufficient alone. The timing report cannot tell a library from a procedural
//! macro, and its `unblocked_units` fields record only the last blocker of each unit rather than the
//! dependency graph. The verbose log carries no timings. The details, and the measurements behind
//! them, are in `docs/cargo-data-sources.md`.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let outputs = cargo_critpath::collect::from_directory(std::path::Path::new("captured"))?;
//! let input = cargo_critpath::parse_with_metadata(
//!     &outputs.verbose_log,
//!     &outputs.timing_html,
//!     outputs.cargo_metadata.as_deref(),
//! )?;
//! println!("{} units, {} edges", input.graph.len(), input.graph.edge_count());
//! # Ok(())
//! # }
//! ```
//!
//! Everything except [`collect`] takes text rather than paths, so it is testable without a
//! filesystem.
#![forbid(unsafe_code)]

pub mod collect;
mod error;
mod input;
mod metadata;
mod timings;
mod units;
mod verbose;

pub use error::{ParseError, UnitDescription};
pub use input::{
    BuildInput, BuildMeta, Diagnostic, Severity, UnblockedCheck, UnblockedPair, UnblockedSource,
};
pub use metadata::{parse as parse_cargo_metadata, PackageIndex};
pub use timings::{parse as parse_timing_report, TimingReport, TimingTarget, TimingUnit};
pub use units::MatchMethod;
pub use verbose::{
    parse as parse_verbose_log, ExternRef, Invocation, PackageRef, Role, VerboseLog,
};

/// Parses one build's verbose log and timing report into an analysable [`BuildInput`].
///
/// This is the entry point most callers want. See [`parse_with_metadata`] for the workspaces where
/// it is not enough.
///
/// # Errors
///
/// Returns [`ParseError`] if either input cannot be parsed, if the build was not a clean full build,
/// or if the two inputs describe different sets of units. No graph is ever built from a partial unit
/// set: an unmatched unit is a refusal, not a warning.
pub fn parse(verbose_log: &str, timing_html: &str) -> Result<BuildInput, ParseError> {
    parse_with_metadata(verbose_log, timing_html, None)
}

/// Parses one build's inputs, resolving workspace-local packages through `cargo metadata`.
///
/// A `rustc` invocation identifies its unit by a source path. For a registry crate that path
/// contains `name-version/` and the package is known exactly. For a workspace-local crate it does
/// not. `cargo metadata --format-version 1` gives every package's `manifest_path`, so a source path
/// can be attributed to a package by longest-prefix match on the manifest directories — exactly,
/// with no heuristic.
///
/// Passing `None` falls back to the crate name and then to the crate's own directory name, which is
/// right for most workspaces and wrong for some: `bevy` keeps several distinct procedural-macro
/// packages in directories all named `macros`. Where the fallback cannot separate two units, the
/// parse fails and the error says that `cargo metadata` output would resolve it.
///
/// # Errors
///
/// As [`parse`], plus [`ParseError::Metadata`] if `cargo_metadata` is not the expected JSON.
pub fn parse_with_metadata(
    verbose_log: &str,
    timing_html: &str,
    cargo_metadata: Option<&str>,
) -> Result<BuildInput, ParseError> {
    let log = verbose::parse(verbose_log)?;
    let report = timings::parse(timing_html)?;
    let packages = match cargo_metadata {
        Some(text) => Some(metadata::parse(text)?),
        None => None,
    };
    input::assemble(&log, &report, packages.as_ref())
}
