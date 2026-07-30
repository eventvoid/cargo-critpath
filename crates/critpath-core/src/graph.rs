//! The build graph: weighted compilation units joined by dependency edges.

use core::fmt;
use std::collections::{BTreeMap, VecDeque};

use crate::error::GraphError;
use crate::millis::Millis;

/// The largest number of units a graph can hold.
///
/// Identifiers are 32 bits so that a graph of a few thousand units stays compact, and the limit is
/// checked rather than allowed to wrap.
pub const MAX_UNITS: usize = u32::MAX as usize - 1;

/// A handle to a unit within one graph.
///
/// Identifiers are only meaningful for the graph that produced them, and mixing them is **not**
/// detected once a graph is built: they are dense indices, so an identifier from another graph
/// either points past the end or silently names a different unit. [`GraphBuilder::add_edge`] does
/// reject an out-of-range identifier as [`GraphError::UnknownUnit`], but that is a bounds check
/// rather than a provenance check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnitId(u32);

impl UnitId {
    /// The identifier as a zero-based index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for UnitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What sort of work a unit performs.
///
/// The simulator does not read this; it exists so that output can describe a unit and so that
/// transformations can refuse to do something meaningless, such as splitting a build script. More
/// variants may be added, so callers must not assume the set is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum UnitKind {
    /// A library that other units compile against.
    Lib,
    /// A procedural macro, which must be fully built and loaded before a dependent can use it.
    ProcMacro,
    /// An executable.
    Bin,
    /// A test or benchmark binary.
    Test,
    /// Compiling a build script into an executable.
    BuildScriptCompile,
    /// Running a build script that has already been compiled.
    BuildScriptRun,
    /// Anything else.
    Other,
}

impl UnitKind {
    /// A short stable name, used in machine-readable output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lib => "lib",
            Self::ProcMacro => "proc-macro",
            Self::Bin => "bin",
            Self::Test => "test",
            Self::BuildScriptCompile => "build-script-compile",
            Self::BuildScriptRun => "build-script-run",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for UnitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One indivisible piece of scheduled work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// The package the unit belongs to.
    pub package: String,
    /// The package's version.
    pub version: String,
    /// The target within the package, such as a library or binary name.
    pub target: String,
    /// What sort of work this is.
    pub kind: UnitKind,
    /// How long the whole unit takes.
    pub total: Millis,
    /// How far into the unit its metadata becomes available, if it produces any.
    ///
    /// `Some` exactly when other units can begin against this one's metadata rather than waiting
    /// for it to finish. Must not exceed `total`.
    pub metadata_at: Option<Millis>,
    /// Whether this unit needs every unit it transitively depends on to be completely finished.
    ///
    /// True for anything that links: an executable, a test, a procedural macro, a dynamic or static
    /// library. Such a unit cannot start until its whole transitive dependency closure is done, not
    /// merely its immediate dependencies, because a linker needs object code that pipelining does
    /// not guarantee to exist yet.
    pub requires_full_closure: bool,
}

/// What a consumer needs from a producer before it can start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    /// The producer's metadata is enough.
    Metadata,
    /// The producer must have finished completely.
    Full,
}

impl EdgeKind {
    /// A short stable name, used in machine-readable output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Full => "full",
        }
    }

    /// Whether this kind releases its consumer no earlier than `other` does.
    #[must_use]
    const fn is_at_least_as_strong_as(self, other: Self) -> bool {
        matches!(self, Self::Full) || matches!(other, Self::Metadata)
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One end of an edge, together with what the edge requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dep {
    /// The unit at the other end.
    pub unit: UnitId,
    /// What the edge requires.
    pub kind: EdgeKind,
}

/// A validated, acyclic build graph.
///
/// Construct one with [`GraphBuilder`]. Once built, a graph is immutable and known to be acyclic,
/// so traversals need no cycle guard.
///
/// ```
/// use critpath_core::{EdgeKind, GraphBuilder, Millis, Unit, UnitKind};
///
/// let mut builder = GraphBuilder::new();
/// let lib = builder.add_unit(Unit {
///     package: "leaf".to_owned(),
///     version: "1.0.0".to_owned(),
///     target: "leaf".to_owned(),
///     kind: UnitKind::Lib,
///     total: Millis::new(800),
///     metadata_at: Some(Millis::new(600)),
///     requires_full_closure: false,
/// })?;
/// let bin = builder.add_unit(Unit {
///     package: "app".to_owned(),
///     version: "1.0.0".to_owned(),
///     target: "app".to_owned(),
///     kind: UnitKind::Bin,
///     total: Millis::new(200),
///     metadata_at: None,
///     requires_full_closure: true,
/// })?;
/// builder.add_edge(lib, bin, EdgeKind::Full)?;
///
/// let graph = builder.build()?;
/// assert_eq!(graph.len(), 2);
/// assert_eq!(graph.total_work(), Millis::new(1_000));
/// # Ok::<(), critpath_core::GraphError>(())
/// ```
#[derive(Clone, Debug)]
pub struct BuildGraph {
    units: Vec<Unit>,
    /// Offsets into `deps`, one per unit plus a trailing total.
    dep_start: Vec<u32>,
    /// Incoming edges, grouped by consumer.
    deps: Vec<Dep>,
    /// Offsets into `rdeps`, one per unit plus a trailing total.
    rdep_start: Vec<u32>,
    /// Outgoing edges, grouped by producer.
    rdeps: Vec<Dep>,
    topo: Vec<UnitId>,
    total_work: Millis,
}

impl BuildGraph {
    /// How many units the graph holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.units.len()
    }

    /// Whether the graph holds no units.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// How many edges the graph holds, after duplicates have been merged.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.deps.len()
    }

    /// The sum of every unit's duration, which is the total work a build must perform.
    #[must_use]
    pub fn total_work(&self) -> Millis {
        self.total_work
    }

    /// The unit `id` refers to, or `None` if the index is outside this graph.
    ///
    /// An identifier belonging to a different graph is not detected: identifiers are dense
    /// indices, so a foreign one reads as `None` only when it points past the end, and otherwise
    /// names a different unit. Do not mix identifiers between graphs.
    #[must_use]
    pub fn unit(&self, id: UnitId) -> Option<&Unit> {
        self.units.get(id.index())
    }

    /// Every unit, in identifier order.
    pub fn units(&self) -> impl ExactSizeIterator<Item = (UnitId, &Unit)> + '_ {
        self.units
            .iter()
            .enumerate()
            .map(|(i, u)| (UnitId(i as u32), u))
    }

    /// The units `id` depends on, or `None` if the index is outside this graph.
    #[must_use]
    pub fn dependencies(&self, id: UnitId) -> Option<&[Dep]> {
        Self::slice(&self.deps, &self.dep_start, id)
    }

    /// The units that depend on `id`, or `None` if the index is outside this graph.
    #[must_use]
    pub fn dependents(&self, id: UnitId) -> Option<&[Dep]> {
        Self::slice(&self.rdeps, &self.rdep_start, id)
    }

    /// Every unit in an order that places each dependency before its dependents.
    #[must_use]
    pub fn topological_order(&self) -> &[UnitId] {
        &self.topo
    }

    fn slice<'a>(edges: &'a [Dep], starts: &[u32], id: UnitId) -> Option<&'a [Dep]> {
        let from = *starts.get(id.index())? as usize;
        let to = *starts.get(id.index().checked_add(1)?)? as usize;
        edges.get(from..to)
    }
}

/// Accumulates units and edges, then validates them into a [`BuildGraph`].
///
/// Identifiers are handed out in insertion order, so an adapter that wants a canonical unit order
/// inserts in that order. The graph never reorders units, so identifiers stay meaningful to the
/// code that built it.
#[derive(Clone, Debug, Default)]
pub struct GraphBuilder {
    units: Vec<Unit>,
    /// Keyed on `(consumer, producer)` so that iteration is ordered and duplicates collapse.
    edges: BTreeMap<(u32, u32), EdgeKind>,
}

impl GraphBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty builder with room for `units` units already reserved.
    #[must_use]
    pub fn with_capacity(units: usize) -> Self {
        Self {
            units: Vec::with_capacity(units),
            edges: BTreeMap::new(),
        }
    }

    /// Adds a unit and returns its identifier.
    ///
    /// # Errors
    ///
    /// [`GraphError::UnitLimitExceeded`] if the graph would exceed [`MAX_UNITS`].
    pub fn add_unit(&mut self, unit: Unit) -> Result<UnitId, GraphError> {
        if self.units.len() >= MAX_UNITS {
            return Err(GraphError::UnitLimitExceeded { limit: MAX_UNITS });
        }
        let id = UnitId(self.units.len() as u32);
        self.units.push(unit);
        Ok(id)
    }

    /// Records that `to` depends on `from`.
    ///
    /// Adding the same pair twice keeps whichever kind releases the consumer later, because
    /// keeping the weaker of two stated constraints would make the analysis optimistic.
    ///
    /// # Errors
    ///
    /// [`GraphError::UnknownUnit`] if either identifier is not from this builder, and
    /// [`GraphError::SelfDependency`] if they are the same unit.
    pub fn add_edge(&mut self, from: UnitId, to: UnitId, kind: EdgeKind) -> Result<(), GraphError> {
        for id in [from, to] {
            if id.index() >= self.units.len() {
                return Err(GraphError::UnknownUnit {
                    id,
                    units: self.units.len(),
                });
            }
        }
        if from == to {
            return Err(GraphError::SelfDependency { unit: from });
        }

        self.edges
            .entry((to.0, from.0))
            .and_modify(|existing| {
                if kind.is_at_least_as_strong_as(*existing) {
                    *existing = kind;
                }
            })
            .or_insert(kind);
        Ok(())
    }

    /// Validates the accumulated units and edges.
    ///
    /// # Errors
    ///
    /// [`GraphError::MetadataAfterCompletion`] if a unit emits metadata after it finishes,
    /// [`GraphError::MetadataEdgeFromOpaqueUnit`] if a metadata edge leaves a unit that emits no
    /// metadata, and [`GraphError::Cycle`] if the dependencies are not acyclic.
    pub fn build(self) -> Result<BuildGraph, GraphError> {
        let count = self.units.len();

        for (i, unit) in self.units.iter().enumerate() {
            if let Some(metadata_at) = unit.metadata_at {
                if metadata_at > unit.total {
                    return Err(GraphError::MetadataAfterCompletion {
                        unit: UnitId(i as u32),
                        metadata_at,
                        total: unit.total,
                    });
                }
            }
        }

        for (&(consumer, producer), &kind) in &self.edges {
            if kind == EdgeKind::Metadata {
                let emits = self
                    .units
                    .get(producer as usize)
                    .is_some_and(|u| u.metadata_at.is_some());
                if !emits {
                    return Err(GraphError::MetadataEdgeFromOpaqueUnit {
                        producer: UnitId(producer),
                        consumer: UnitId(consumer),
                    });
                }
            }
        }

        let mut in_degree = vec![0u32; count];
        let mut out_degree = vec![0u32; count];
        for &(consumer, producer) in self.edges.keys() {
            if let Some(d) = in_degree.get_mut(consumer as usize) {
                *d = d.saturating_add(1);
            }
            if let Some(d) = out_degree.get_mut(producer as usize) {
                *d = d.saturating_add(1);
            }
        }

        // Incoming edges, grouped by consumer. `self.edges` is keyed on `(consumer, producer)`, so
        // iterating it in order visits every edge of consumer 0, then every edge of consumer 1, and
        // so on. Pushing sequentially therefore lands each edge exactly where the prefix sums of
        // the in-degrees say it belongs.
        let dep_start = prefix_sums(&in_degree);
        let deps: Vec<Dep> = self
            .edges
            .iter()
            .map(|(&(_, producer), &kind)| Dep {
                unit: UnitId(producer),
                kind,
            })
            .collect();

        // Outgoing edges, grouped by producer. Iteration order does not group by producer, so these
        // are placed through a moving cursor per producer.
        let rdep_start = prefix_sums(&out_degree);
        let mut cursor = rdep_start.clone();
        let mut rdeps = vec![
            Dep {
                unit: UnitId(0),
                kind: EdgeKind::Full,
            };
            self.edges.len()
        ];
        for (&(consumer, producer), &kind) in &self.edges {
            let Some(slot) = cursor.get_mut(producer as usize) else {
                continue;
            };
            let at = *slot as usize;
            *slot = slot.saturating_add(1);
            if let Some(cell) = rdeps.get_mut(at) {
                *cell = Dep {
                    unit: UnitId(consumer),
                    kind,
                };
            }
        }

        let topo = topological_order(count, &in_degree, &rdep_start, &rdeps);
        if topo.len() < count {
            return Err(GraphError::Cycle {
                units: find_cycle(count, &topo, &dep_start, &deps),
            });
        }

        let total_work = self.units.iter().map(|u| u.total).sum();

        Ok(BuildGraph {
            units: self.units,
            dep_start,
            deps,
            rdep_start,
            rdeps,
            topo,
            total_work,
        })
    }
}

/// Offsets of each group in a compressed adjacency list, plus a trailing total.
fn prefix_sums(degrees: &[u32]) -> Vec<u32> {
    let mut starts = Vec::with_capacity(degrees.len().saturating_add(1));
    let mut running = 0u32;
    for degree in degrees {
        starts.push(running);
        running = running.saturating_add(*degree);
    }
    starts.push(running);
    starts
}

/// Kahn's algorithm, seeded in ascending identifier order so the result is deterministic.
fn topological_order(
    count: usize,
    in_degree: &[u32],
    rdep_start: &[u32],
    rdeps: &[Dep],
) -> Vec<UnitId> {
    let mut remaining = in_degree.to_vec();
    let mut order = Vec::with_capacity(count);
    let mut queue: VecDeque<u32> = (0..count)
        .filter(|&i| remaining.get(i).copied() == Some(0))
        .map(|i| i as u32)
        .collect();

    while let Some(node) = queue.pop_front() {
        order.push(UnitId(node));
        let from = *rdep_start.get(node as usize).unwrap_or(&0) as usize;
        let to = rdep_start
            .get((node as usize).saturating_add(1))
            .copied()
            .unwrap_or(0) as usize;
        for dep in rdeps.get(from..to).unwrap_or(&[]) {
            if let Some(d) = remaining.get_mut(dep.unit.index()) {
                *d = d.saturating_sub(1);
                if *d == 0 {
                    queue.push_back(dep.unit.0);
                }
            }
        }
    }

    order
}

/// Walks backwards along dependencies among the units Kahn's algorithm could not place, which is
/// exactly the set that lies on or behind a cycle, until a unit repeats.
fn find_cycle(count: usize, placed: &[UnitId], dep_start: &[u32], deps: &[Dep]) -> Vec<UnitId> {
    let mut stuck = vec![true; count];
    for id in placed {
        if let Some(slot) = stuck.get_mut(id.index()) {
            *slot = false;
        }
    }

    let Some(start) = (0..count).find(|&i| stuck.get(i).copied() == Some(true)) else {
        return Vec::new();
    };

    let mut on_path = vec![usize::MAX; count];
    let mut path: Vec<u32> = Vec::new();
    let mut current = start as u32;

    loop {
        if let Some(&position) = on_path.get(current as usize) {
            if position != usize::MAX {
                let mut cycle: Vec<UnitId> = path
                    .get(position..)
                    .unwrap_or(&[])
                    .iter()
                    .map(|&n| UnitId(n))
                    .collect();
                // The walk followed edges from consumer to dependency; reverse it so the reported
                // order runs from a dependency to the unit that depends on it.
                cycle.reverse();
                return cycle;
            }
        }
        if let Some(slot) = on_path.get_mut(current as usize) {
            *slot = path.len();
        }
        path.push(current);

        let from = *dep_start.get(current as usize).unwrap_or(&0) as usize;
        let to = dep_start
            .get((current as usize).saturating_add(1))
            .copied()
            .unwrap_or(0) as usize;
        let next = deps
            .get(from..to)
            .unwrap_or(&[])
            .iter()
            .filter(|d| stuck.get(d.unit.index()).copied() == Some(true))
            .map(|d| d.unit.0)
            .min();

        match next {
            Some(n) => current = n,
            // Cannot happen for a unit Kahn's algorithm left in place, but returning what was
            // found is better than looping.
            None => return path.iter().map(|&n| UnitId(n)).collect(),
        }
    }
}
