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
//! Everything here takes text rather than paths, so it is testable without a filesystem.
#![forbid(unsafe_code)]

mod error;
mod timings;
mod verbose;

pub use error::{ParseError, UnitDescription};
pub use timings::{parse as parse_timing_report, TimingReport, TimingTarget, TimingUnit};
pub use verbose::{
    parse as parse_verbose_log, ExternRef, Invocation, PackageRef, Role, VerboseLog,
};
