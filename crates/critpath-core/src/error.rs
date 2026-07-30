//! Errors reported when a graph cannot be built.

use core::fmt;

use crate::graph::UnitId;
use crate::millis::Millis;

/// A graph that could not be constructed, and why.
///
/// Every variant names the units involved, because the caller building the graph is a program
/// translating some build system's output and needs to know which piece of that output was wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphError {
    /// More units were added than can be addressed.
    UnitLimitExceeded {
        /// The largest number of units supported.
        limit: usize,
    },
    /// An edge named a unit that was never added.
    UnknownUnit {
        /// The identifier that does not belong to this graph.
        id: UnitId,
        /// How many units the graph does contain.
        units: usize,
    },
    /// A unit was given itself as a dependency.
    SelfDependency {
        /// The unit that depends on itself.
        unit: UnitId,
    },
    /// A metadata edge leaves a unit that never emits metadata.
    MetadataEdgeFromOpaqueUnit {
        /// The unit the edge leaves, which has no `metadata_at`.
        producer: UnitId,
        /// The unit the edge arrives at.
        consumer: UnitId,
    },
    /// A unit claims to emit its metadata after it has already finished.
    MetadataAfterCompletion {
        /// The offending unit.
        unit: UnitId,
        /// When it claims to emit metadata.
        metadata_at: Millis,
        /// How long it runs in total.
        total: Millis,
    },
    /// The dependencies contain a cycle, so no build order exists.
    Cycle {
        /// The units forming one cycle, in dependency order. There may be others.
        units: Vec<UnitId>,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnitLimitExceeded { limit } => write!(
                f,
                "the graph cannot hold more than {limit} units; \
                 analyse a subset of the workspace instead"
            ),
            Self::UnknownUnit { id, units } => write!(
                f,
                "unit {id} was named by an edge but never added to this graph, \
                 which holds {units} units; \
                 identifiers are only valid for the graph that produced them"
            ),
            Self::SelfDependency { unit } => write!(
                f,
                "unit {unit} depends on itself, so it can never start; \
                 drop the edge, since a unit is always available to itself"
            ),
            Self::MetadataEdgeFromOpaqueUnit { producer, consumer } => write!(
                f,
                "the edge from unit {producer} to unit {consumer} is a metadata edge, \
                 but unit {producer} emits no metadata; \
                 make it a full edge, or give unit {producer} a metadata_at time"
            ),
            Self::MetadataAfterCompletion {
                unit,
                metadata_at,
                total,
            } => write!(
                f,
                "unit {unit} emits metadata at {metadata_at} but finishes after {total}; \
                 metadata cannot be emitted after the unit that emits it has ended, \
                 so check the parser that produced these two numbers"
            ),
            Self::Cycle { units } => {
                write!(
                    f,
                    "the dependencies contain a cycle, so no build order exists: "
                )?;
                for (i, unit) in units.iter().enumerate() {
                    if i > 0 {
                        write!(f, " -> ")?;
                    }
                    write!(f, "unit {unit}")?;
                }
                write!(
                    f,
                    "; a build graph is acyclic, so this is a defect in whatever produced it"
                )
            }
        }
    }
}

impl std::error::Error for GraphError {}
