//! Schedule simulation and counterfactual analysis over a weighted build DAG.
//!
//! This crate knows nothing about Cargo, `rustc`, files, or JSON. It takes a graph of weighted
//! compilation units joined by dependency edges, and answers two kinds of question about it:
//! how long the build must take, and what a change to the graph would do to that number.
//!
//! Using it for a build system other than Cargo means writing an adapter that produces a
//! [`BuildGraph`]; nothing in here has to change.
//!
//! The model, its invariants, and its known systematic errors are documented in `docs/model.md`
//! in the repository.

#![forbid(unsafe_code)]

mod error;
mod graph;
mod millis;

pub use error::GraphError;
pub use graph::{BuildGraph, Dep, EdgeKind, GraphBuilder, Unit, UnitId, UnitKind, MAX_UNITS};
pub use millis::Millis;
