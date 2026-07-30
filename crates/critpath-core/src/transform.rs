//! Counterfactual transformations: hypothetical edits to a graph, each producing a new graph.
//!
//! A transformation answers "what if". It does not compute a saving and does not decide whether a
//! change is worth making; it produces the graph the change would give, so that the metrics can be
//! recomputed on it and the two sets of numbers compared by the caller.
//!
//! Every result is built through [`GraphBuilder`], so a transformed graph is validated and acyclic
//! exactly like an original one. Identifiers are therefore **not** carried across: the new graph
//! hands out its own, and [`Applied`] holds the map from the old ones to the new.

use core::fmt;
use core::num::NonZeroU64;

use crate::error::GraphError;
use crate::graph::{BuildGraph, GraphBuilder, Unit, UnitId, UnitKind};
use crate::millis::Millis;

/// What the first part of a split unit is called, appended to the original target name.
const PART_A: &str = " (part a)";
/// What the second part of a split unit is called, appended to the original target name.
const PART_B: &str = " (part b)";

/// Which part of a split unit each of its dependents is assumed to need.
///
/// Splitting a crate cannot be modelled exactly, because which half a dependent actually needs is
/// not knowable without performing the split. The two variants bracket that unknown rather than
/// guessing at it.
///
/// The bracket is often degenerate: at a division fraction of one half the two parts finish at the
/// same instant, so which dependent attaches to which part cannot move any downstream start time
/// and the two answers coincide exactly. That is a property of the model, not a defect, and it is
/// why the dependent assumption is not the only thing a reported interval should be swept over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SplitDependents {
    /// Each dependent needs exactly one part, and it is the part that finishes first: the one
    /// carrying less of the work, or part a when the two are equal. The best a split could do.
    ///
    /// Both parts inherit the same incoming edges and so can start at the same moment, which means
    /// the smaller part reaches both its metadata and its end no later than the larger one.
    /// Attaching every dependent to it therefore minimises every downstream start time at once,
    /// whether an edge waits for metadata or for the whole part. No other assignment gives a
    /// shorter critical path, which is what makes this a bound rather than one guess among many.
    ///
    /// It is not a plausible refactor: a part that nothing depends on would not be written. It is
    /// the limit that a plausible refactor cannot beat.
    Optimistic,
    /// Every dependent still needs both parts. The split buys nothing structurally, and only the
    /// pipelining boundary moves.
    Pessimistic,
}

/// A hypothetical edit to a build graph.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Transform {
    /// Divide one unit into two, as splitting a crate in a refactor would.
    ///
    /// The first part receives `numerator / denominator` of the unit's work, rounded to nearest,
    /// and the second receives the remainder, so the two parts sum to exactly the original weight.
    /// `overhead` is then added to each part, because two crates re-parse shared imports and each
    /// pays a fixed per-crate cost.
    ///
    /// A part's `metadata_at` is scaled by that part's share of the denominator and then clamped
    /// so that it never exceeds the part's own total. The clamp is reachable through rounding
    /// alone: a unit of 999ms that emits metadata at 999ms, divided at one half, gives parts of
    /// 500ms and 499ms while both metadata times round to 500ms, and the second is clamped to
    /// 499ms. `overhead` is added to the total but not to `metadata_at`, so the metadata boundary
    /// of a part is the earliest the split could plausibly reach.
    ///
    /// Both parts inherit every incoming edge with its original kind, along with `kind` and
    /// `requires_full_closure`. Where the outgoing edges go is decided by `dependents`:
    /// [`SplitDependents::Pessimistic`] gives every dependent an edge from both parts, and
    /// [`SplitDependents::Optimistic`] gives each dependent one edge, from the part that finishes
    /// first.
    ///
    /// A build script cannot be split: one invocation of one program is not divisible, and
    /// `numerator` may not exceed `denominator`.
    SplitUnit {
        /// The unit to divide.
        unit: UnitId,
        /// The numerator of the fraction of the work the first part receives.
        numerator: u64,
        /// The denominator of that fraction.
        denominator: NonZeroU64,
        /// Which part each dependent is assumed to need.
        dependents: SplitDependents,
        /// The fixed cost each part pays for being a separate crate.
        overhead: Millis,
    },
    /// Remove one dependency edge, as deleting a `use` of one crate from another would.
    ///
    /// Only the edge goes; both units stay, even if one of them is left with nothing depending on
    /// it. Removing a unit is [`Transform::RemoveUnits`].
    RemoveEdge {
        /// The producer the edge leaves.
        from: UnitId,
        /// The consumer the edge arrives at.
        to: UnitId,
    },
    /// Remove units, as dropping a dependency or turning off a feature would.
    ///
    /// Removing a unit also removes everything that existed only to serve it. Precisely: let a
    /// *root* be a unit that had no dependents in the **original** graph, which is what the build
    /// was asked to produce. After the named units are removed, every surviving unit that can no
    /// longer reach a surviving root by following dependents is removed as well. Reachability is
    /// reflexive, so a surviving root reaches itself and always survives.
    ///
    /// Naming a unit twice is not an error, and removing every unit gives a valid empty graph.
    RemoveUnits {
        /// The units to remove.
        units: Vec<UnitId>,
    },
}

/// What became of one unit of the original graph.
///
/// Match with a wildcard arm, or use [`Mapped::primary`] and [`Mapped::secondary`]: a later
/// transformation may map a unit in a way that does not exist yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mapped {
    /// The unit survived unchanged, under a new identifier.
    Kept(UnitId),
    /// The unit was divided in two.
    Split {
        /// The part that received `numerator / denominator` of the work.
        first: UnitId,
        /// The part that received the remainder.
        second: UnitId,
    },
    /// The unit is not in the new graph.
    Removed,
}

impl Mapped {
    /// The new identifier that best stands for the original unit, or `None` if it was removed.
    ///
    /// For a split, this is the first part.
    #[must_use]
    pub const fn primary(self) -> Option<UnitId> {
        match self {
            Self::Kept(id) => Some(id),
            Self::Split { first, .. } => Some(first),
            _ => None,
        }
    }

    /// The second new identifier, for a mapping that produced two units.
    #[must_use]
    pub const fn secondary(self) -> Option<UnitId> {
        match self {
            Self::Split { second, .. } => Some(second),
            _ => None,
        }
    }

    /// Whether the unit is absent from the new graph.
    #[must_use]
    pub const fn is_removed(self) -> bool {
        matches!(self, Self::Removed)
    }
}

/// The graph a transformation produced, and where each original unit went.
///
/// The new graph is a different graph, so identifiers from the original are meaningless in it.
/// Anything that wants to talk about the same unit before and after — a comparison of critical
/// paths, a report that names the crate it just moved — has to go through the mapping.
#[derive(Clone, Debug)]
pub struct Applied {
    graph: BuildGraph,
    /// One entry per unit of the original graph, in original identifier order.
    mapping: Vec<(UnitId, Mapped)>,
}

impl Applied {
    /// The transformed graph.
    #[must_use]
    pub fn graph(&self) -> &BuildGraph {
        &self.graph
    }

    /// The transformed graph, given up by value.
    #[must_use]
    pub fn into_graph(self) -> BuildGraph {
        self.graph
    }

    /// What became of `original`, or `None` if it is not a unit of the graph that was transformed.
    #[must_use]
    pub fn mapped(&self, original: UnitId) -> Option<Mapped> {
        self.mapping
            .get(original.index())
            .map(|&(_, mapped)| mapped)
    }

    /// Every unit of the original graph and what became of it, in original identifier order.
    pub fn mapping(&self) -> impl ExactSizeIterator<Item = (UnitId, Mapped)> + '_ {
        self.mapping.iter().copied()
    }
}

/// A transformation that could not be applied, and why.
///
/// Every variant names the units involved, because the caller is usually acting on a unit a user
/// picked out of a report and needs to say which one was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransformError {
    /// A transformation named a unit that is not in the graph it was applied to.
    UnknownUnit {
        /// The identifier that does not belong to this graph.
        unit: UnitId,
        /// How many units the graph does contain.
        units: usize,
    },
    /// A transformation tried to split a unit that is not divisible.
    UnsplittableKind {
        /// The unit that cannot be split.
        unit: UnitId,
        /// What sort of work it performs.
        kind: UnitKind,
    },
    /// A split asked for more work than the unit contains.
    FractionAboveOne {
        /// The unit that would have been split.
        unit: UnitId,
        /// The numerator that was asked for.
        numerator: u64,
        /// The denominator it was over.
        denominator: NonZeroU64,
    },
    /// An edge removal named an edge the graph does not have.
    MissingEdge {
        /// The producer the edge was supposed to leave.
        from: UnitId,
        /// The consumer it was supposed to arrive at.
        to: UnitId,
    },
    /// The graph the transformation produced did not validate.
    Rebuild {
        /// Why the produced graph was rejected.
        source: GraphError,
    },
}

impl From<GraphError> for TransformError {
    fn from(source: GraphError) -> Self {
        Self::Rebuild { source }
    }
}

impl fmt::Display for TransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownUnit { unit, units } => write!(
                f,
                "unit {unit} is not part of this graph, which holds {units} units; \
                 identifiers are only valid for the graph that handed them out, \
                 so take the identifier from the graph being transformed"
            ),
            Self::UnsplittableKind { unit, kind } => write!(
                f,
                "unit {unit} is a {kind} and cannot be split, \
                 because one run of one build script is a single indivisible step; \
                 split one of the library units instead, \
                 or shorten the build script by other means"
            ),
            Self::FractionAboveOne {
                unit,
                numerator,
                denominator,
            } => write!(
                f,
                "the division fraction {numerator}/{denominator} for unit {unit} is more than the \
                 whole unit, which would leave the second part with negative work; \
                 choose a numerator no larger than the denominator"
            ),
            Self::MissingEdge { from, to } => write!(
                f,
                "there is no edge from unit {from} to unit {to}, so it cannot be removed; \
                 list the dependencies of unit {to} and name one of those, \
                 remembering that an edge runs from the producer to the consumer"
            ),
            Self::Rebuild { source } => write!(
                f,
                "the graph this transformation produced did not validate: {source}"
            ),
        }
    }
}

impl std::error::Error for TransformError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rebuild { source } => Some(source),
            _ => None,
        }
    }
}

/// The units nothing depends on: what the build was asked to produce.
///
/// Returned in ascending identifier order.
///
/// ```
/// use critpath_core::{roots, EdgeKind, GraphBuilder, Millis, Unit, UnitKind};
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
/// let graph = builder.build()?;
///
/// // Something depends on the library; nothing depends on the binary.
/// assert_eq!(roots(&graph), vec![bin]);
/// # Ok::<(), critpath_core::GraphError>(())
/// ```
#[must_use]
pub fn roots(graph: &BuildGraph) -> Vec<UnitId> {
    graph
        .units()
        .filter(|&(id, _)| is_root(graph, id))
        .map(|(id, _)| id)
        .collect()
}

/// Applies `transform` to `graph`, leaving `graph` untouched.
///
/// Each transformation copies the graph once, visiting every unit and edge a constant number of
/// times. Handing the edges to [`GraphBuilder`] puts each one into an ordered map, so the cost is
/// `O(V + E log E)`, of which validating the result is `O(V + E)`.
///
/// # Errors
///
/// [`TransformError::UnknownUnit`] if the transformation names a unit of another graph,
/// [`TransformError::UnsplittableKind`] and [`TransformError::FractionAboveOne`] if a split is
/// impossible or asks for more than the whole unit, [`TransformError::MissingEdge`] if an edge
/// removal names an edge that is not there, and [`TransformError::Rebuild`] if the produced graph
/// is rejected, which for a split of a graph already at [`crate::MAX_UNITS`] means there is no
/// room for the extra unit.
///
/// ```
/// use core::num::NonZeroU64;
/// use critpath_core::{
///     apply, GraphBuilder, Millis, SplitDependents, Transform, Unit, UnitKind,
/// };
///
/// let mut builder = GraphBuilder::new();
/// let heavy = builder.add_unit(Unit {
///     package: "heavy".to_owned(),
///     version: "1.0.0".to_owned(),
///     target: "heavy".to_owned(),
///     kind: UnitKind::Lib,
///     total: Millis::new(1_000),
///     metadata_at: Some(Millis::new(700)),
///     requires_full_closure: false,
/// })?;
/// let graph = builder.build()?;
///
/// let applied = apply(
///     &graph,
///     &Transform::SplitUnit {
///         unit: heavy,
///         numerator: 1,
///         denominator: NonZeroU64::new(2).ok_or("2 is not zero")?,
///         dependents: SplitDependents::Pessimistic,
///         overhead: Millis::new(50),
///     },
/// )?;
///
/// // 1000ms divided in half is 500ms in each part, and each part pays the 50ms overhead.
/// assert_eq!(applied.graph().len(), 2);
/// assert_eq!(applied.graph().total_work(), Millis::new(1_100));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn apply(graph: &BuildGraph, transform: &Transform) -> Result<Applied, TransformError> {
    match transform {
        Transform::SplitUnit {
            unit,
            numerator,
            denominator,
            dependents,
            overhead,
        } => split_unit(
            graph,
            *unit,
            *numerator,
            *denominator,
            *dependents,
            *overhead,
        ),
        Transform::RemoveEdge { from, to } => remove_edge(graph, *from, *to),
        Transform::RemoveUnits { units } => remove_units(graph, units),
    }
}

fn split_unit(
    graph: &BuildGraph,
    unit: UnitId,
    numerator: u64,
    denominator: NonZeroU64,
    dependents: SplitDependents,
    overhead: Millis,
) -> Result<Applied, TransformError> {
    let original = graph.unit(unit).ok_or(TransformError::UnknownUnit {
        unit,
        units: graph.len(),
    })?;
    if matches!(
        original.kind,
        UnitKind::BuildScriptCompile | UnitKind::BuildScriptRun
    ) {
        return Err(TransformError::UnsplittableKind {
            unit,
            kind: original.kind,
        });
    }
    if numerator > denominator.get() {
        return Err(TransformError::FractionAboveOne {
            unit,
            numerator,
            denominator,
        });
    }

    // The second part is the remainder rather than a second rounded multiplication, so that the
    // two parts sum to exactly the original weight and a split moves work without inventing any.
    let first_work = original.total.scaled(numerator, denominator);
    let second_work = original.total.saturating_sub(first_work);
    let second_share = denominator.get().saturating_sub(numerator);

    let part = |work: Millis, share: u64, suffix: &str| {
        let total = work.saturating_add(overhead);
        Unit {
            package: original.package.clone(),
            // The parts are hypothetical, so they are named after the unit they came from. No
            // crate by this name exists.
            target: format!("{}{suffix}", original.target),
            version: original.version.clone(),
            kind: original.kind,
            total,
            // Rounding can push a scaled metadata time past the part that carries it, and a unit
            // that emits metadata after it has finished is not a graph the builder accepts.
            metadata_at: original
                .metadata_at
                .map(|at| at.scaled(share, denominator).min(total)),
            requires_full_closure: original.requires_full_closure,
        }
    };
    let first_unit = part(first_work, numerator, PART_A);
    let second_unit = part(second_work, second_share, PART_B);

    // Under the optimistic assumption every dependent needs one part, and it is the part that
    // finishes first. Both parts inherit the same incoming edges, so they can start at the same
    // moment, and the one carrying less work then reaches both its metadata and its end no later
    // than the other. Attaching every dependent to it therefore minimises every downstream start
    // time at once, whatever kind each edge is, which is what makes this a bound rather than one
    // arbitrary assignment among many.
    let dependents_take_first = first_unit.total <= second_unit.total;

    let mut builder = GraphBuilder::with_capacity(graph.len().saturating_add(1));
    let mut mapping = Vec::with_capacity(graph.len());
    let mut parts = None;
    for (old, existing) in graph.units() {
        if old == unit {
            // The parts take the position the original held, so the order of everything else is
            // preserved.
            let first = builder.add_unit(first_unit.clone())?;
            let second = builder.add_unit(second_unit.clone())?;
            parts = Some((first, second));
            mapping.push((old, Mapped::Split { first, second }));
        } else {
            let new = builder.add_unit(existing.clone())?;
            mapping.push((old, Mapped::Kept(new)));
        }
    }
    let Some((first, second)) = parts else {
        return Err(TransformError::UnknownUnit {
            unit,
            units: graph.len(),
        });
    };

    let units = graph.len();
    for (consumer, _) in graph.units() {
        for dep in graph.dependencies(consumer).unwrap_or(&[]) {
            if consumer == unit {
                // Both parts inherit every incoming edge: the work they share came from one crate,
                // and nothing says which part of it needed which dependency.
                let from = translate(&mapping, dep.unit, units)?;
                builder.add_edge(from, first, dep.kind)?;
                builder.add_edge(from, second, dep.kind)?;
            } else if dep.unit == unit {
                let to = translate(&mapping, consumer, units)?;
                match dependents {
                    SplitDependents::Pessimistic => {
                        builder.add_edge(first, to, dep.kind)?;
                        builder.add_edge(second, to, dep.kind)?;
                    }
                    SplitDependents::Optimistic => {
                        let from = if dependents_take_first { first } else { second };
                        builder.add_edge(from, to, dep.kind)?;
                    }
                }
            } else {
                let from = translate(&mapping, dep.unit, units)?;
                let to = translate(&mapping, consumer, units)?;
                builder.add_edge(from, to, dep.kind)?;
            }
        }
    }

    Ok(Applied {
        graph: builder.build()?,
        mapping,
    })
}

fn remove_edge(graph: &BuildGraph, from: UnitId, to: UnitId) -> Result<Applied, TransformError> {
    for id in [from, to] {
        if graph.unit(id).is_none() {
            return Err(TransformError::UnknownUnit {
                unit: id,
                units: graph.len(),
            });
        }
    }

    let present = graph
        .dependencies(to)
        .unwrap_or(&[])
        .iter()
        .any(|dep| dep.unit == from);
    if !present {
        return Err(TransformError::MissingEdge { from, to });
    }

    rebuild(graph, &vec![true; graph.len()], Some((from, to)))
}

fn remove_units(graph: &BuildGraph, targets: &[UnitId]) -> Result<Applied, TransformError> {
    let mut survives = vec![true; graph.len()];
    for &target in targets {
        match survives.get_mut(target.index()) {
            Some(slot) => *slot = false,
            None => {
                return Err(TransformError::UnknownUnit {
                    unit: target,
                    units: graph.len(),
                })
            }
        }
    }

    // A unit is worth building only if something the build was asked to produce still needs it.
    // The roots are taken from the original graph, because removing a unit does not turn whatever
    // it depended on into a thing the build was asked for.
    let mut reaches_root = vec![false; graph.len()];
    let mut pending: Vec<UnitId> = Vec::new();
    for (id, _) in graph.units() {
        if is_root(graph, id) && survives.get(id.index()).copied().unwrap_or(false) {
            if let Some(slot) = reaches_root.get_mut(id.index()) {
                *slot = true;
            }
            pending.push(id);
        }
    }
    while let Some(id) = pending.pop() {
        for dep in graph.dependencies(id).unwrap_or(&[]) {
            if !survives.get(dep.unit.index()).copied().unwrap_or(false) {
                continue;
            }
            if let Some(slot) = reaches_root.get_mut(dep.unit.index()) {
                if !*slot {
                    *slot = true;
                    pending.push(dep.unit);
                }
            }
        }
    }

    rebuild(graph, &reaches_root, None)
}

/// Copies the units `keep` marks, in their original relative order, and every edge whose two ends
/// both survive and which `drop_edge` does not name.
fn rebuild(
    graph: &BuildGraph,
    keep: &[bool],
    drop_edge: Option<(UnitId, UnitId)>,
) -> Result<Applied, TransformError> {
    let mut builder = GraphBuilder::with_capacity(graph.len());
    let mut mapping = Vec::with_capacity(graph.len());
    for (old, existing) in graph.units() {
        if keep.get(old.index()).copied().unwrap_or(false) {
            let new = builder.add_unit(existing.clone())?;
            mapping.push((old, Mapped::Kept(new)));
        } else {
            mapping.push((old, Mapped::Removed));
        }
    }

    for (consumer, _) in graph.units() {
        for dep in graph.dependencies(consumer).unwrap_or(&[]) {
            if drop_edge == Some((dep.unit, consumer)) {
                continue;
            }
            let (Some(from), Some(to)) = (kept(&mapping, dep.unit), kept(&mapping, consumer))
            else {
                continue;
            };
            builder.add_edge(from, to, dep.kind)?;
        }
    }

    Ok(Applied {
        graph: builder.build()?,
        mapping,
    })
}

/// Whether nothing depends on `id`.
fn is_root(graph: &BuildGraph, id: UnitId) -> bool {
    match graph.dependents(id) {
        Some(dependents) => dependents.is_empty(),
        None => false,
    }
}

/// The new identifier of a unit that survived unchanged.
fn kept(mapping: &[(UnitId, Mapped)], old: UnitId) -> Option<UnitId> {
    match mapping.get(old.index()) {
        Some(&(_, Mapped::Kept(new))) => Some(new),
        _ => None,
    }
}

/// As [`kept`], reporting the identifier that could not be translated rather than dropping it.
fn translate(
    mapping: &[(UnitId, Mapped)],
    old: UnitId,
    units: usize,
) -> Result<UnitId, TransformError> {
    kept(mapping, old).ok_or(TransformError::UnknownUnit { unit: old, units })
}
