//! Earliest and latest times over a build graph.
//!
//! This is the part of the tool that does not depend on how many executors a build gets: the
//! critical path length, and how much room every unit beside it has. Section 4 of `docs/model.md`
//! defines what is computed here and section 7 lists the invariants the tests cite.

use crate::graph::{BuildGraph, EdgeKind, UnitId};
use crate::millis::Millis;

/// Earliest and latest times for every unit of one graph, and the critical path through it.
///
/// Produced by [`analyze`]. Identifiers from a different graph read as `None` rather than as some
/// other unit's numbers.
///
/// ```
/// use critpath_core::{analyze, EdgeKind, GraphBuilder, Millis, Unit, UnitKind};
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
/// builder.add_edge(lib, bin, EdgeKind::Metadata)?;
/// let graph = builder.build()?;
///
/// let analysis = analyze(&graph);
/// // The edge offers metadata at 600, but a binary has to link, so it waits for all 800.
/// assert_eq!(analysis.earliest_start(bin), Some(Millis::new(800)));
/// assert_eq!(analysis.critical_path_len(), Millis::new(1_000));
/// assert_eq!(analysis.critical_path(), &[lib, bin]);
/// # Ok::<(), critpath_core::GraphError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Analysis {
    earliest_start: Vec<Millis>,
    earliest_finish: Vec<Millis>,
    latest_finish: Vec<Millis>,
    /// What each unit adds to the reported critical path, and `None` for a unit not on it.
    contribution: Vec<Option<Millis>>,
    path: Vec<UnitId>,
    critical_path_len: Millis,
}

/// The constraint that decided a unit's earliest start, and which unit imposed it.
///
/// `Metadata` means the path traverses the producer at its metadata offset; `Full` means it
/// traverses the producer's whole duration. A closure requirement is recorded as `Full`, because
/// that is what it demands, whatever kind the intervening edges had.
#[derive(Clone, Copy)]
struct Reason {
    producer: UnitId,
    kind: EdgeKind,
}

impl Analysis {
    /// The critical path length: how long the build takes with as many executors as it wants.
    ///
    /// Zero for an empty graph. Never depends on the executor count or on a scheduling policy
    /// (I10), which is why it is the number to lead with.
    #[must_use]
    pub fn critical_path_len(&self) -> Millis {
        self.critical_path_len
    }

    /// One longest path, from a unit with no dependencies to the unit that finishes last.
    ///
    /// Empty for an empty graph. Where several paths are equally long, the reported one is fixed by
    /// identifier order so that output is reproducible (I1).
    ///
    /// Consecutive units need not be joined by an edge: a unit that requires its whole closure is
    /// preceded on the path by the unit in that closure which held it back, which section 3.1 of
    /// `docs/model.md` shows need not be a direct dependency.
    #[must_use]
    pub fn critical_path(&self) -> &[UnitId] {
        &self.path
    }

    /// The earliest a unit could start, or `None` for an identifier from another graph.
    #[must_use]
    pub fn earliest_start(&self, id: UnitId) -> Option<Millis> {
        self.earliest_start.get(id.index()).copied()
    }

    /// The earliest a unit could finish, or `None` for an identifier from another graph.
    #[must_use]
    pub fn earliest_finish(&self, id: UnitId) -> Option<Millis> {
        self.earliest_finish.get(id.index()).copied()
    }

    /// The latest a unit may finish without lengthening the critical path, or `None` for an
    /// identifier from another graph.
    #[must_use]
    pub fn latest_finish(&self, id: UnitId) -> Option<Millis> {
        self.latest_finish.get(id.index()).copied()
    }

    /// How much later than its earliest finish a unit may finish, or `None` for an identifier from
    /// another graph.
    ///
    /// Zero exactly for the units that cannot move at all (I9). Note that this is room in the
    /// *finish* time: lengthening a pipelined producer by its slack is safe, but so is lengthening
    /// it further, because a consumer released by metadata never waited for the finish anyway.
    #[must_use]
    pub fn slack(&self, id: UnitId) -> Option<Millis> {
        let latest = self.latest_finish(id)?;
        let earliest = self.earliest_finish(id)?;
        Some(latest.saturating_sub(earliest))
    }

    /// Whether a unit has no slack at all, so that delaying it lengthens the whole build.
    ///
    /// True for every unit of [`critical_path`](Self::critical_path), and also for units on any
    /// other equally long path.
    #[must_use]
    pub fn is_critical(&self, id: UnitId) -> bool {
        self.slack(id) == Some(Millis::ZERO)
    }

    /// What a unit adds to the length of [`critical_path`](Self::critical_path).
    ///
    /// A unit traversed over a metadata edge adds only its metadata offset, because that is all the
    /// path waited for; otherwise it adds its whole duration. The contributions of the units on the
    /// path sum to exactly [`critical_path_len`](Self::critical_path_len), which makes them the
    /// right quantity to rank candidates for change by.
    ///
    /// `None` for a unit that is not on the reported path, and for an identifier from another
    /// graph. A unit off the path adds nothing to it, but that does not mean the build would not
    /// notice its absence: check [`is_critical`](Self::is_critical) for units on an equally long
    /// path elsewhere.
    #[must_use]
    pub fn critical_contribution(&self, id: UnitId) -> Option<Millis> {
        self.contribution.get(id.index()).copied().flatten()
    }
}

/// Computes earliest times, the critical path, and slack, in `O(V + E)`.
///
/// One pass forward in topological order and one back, each visiting every edge once. Nothing here
/// consults an executor count.
///
/// See [`Analysis`] for an example.
#[must_use]
pub fn analyze(graph: &BuildGraph) -> Analysis {
    let count = graph.len();
    let (earliest_start, earliest_finish, reason) = forward_pass(graph, count);
    let (critical_path_len, attaining) = latest_finisher(graph, &earliest_finish);
    let latest_finish = backward_pass(graph, count, critical_path_len, &earliest_finish);
    let (path, contribution) = recover_path(graph, count, attaining, &reason);

    Analysis {
        earliest_start,
        earliest_finish,
        latest_finish,
        contribution,
        path,
        critical_path_len,
    }
}

/// Earliest start and finish for every unit, with the constraint that decided each start.
fn forward_pass(
    graph: &BuildGraph,
    count: usize,
) -> (Vec<Millis>, Vec<Millis>, Vec<Option<Reason>>) {
    let mut earliest_start = vec![Millis::ZERO; count];
    let mut earliest_finish = vec![Millis::ZERO; count];
    let mut reason: Vec<Option<Reason>> = vec![None; count];
    // The latest finish anywhere in a unit's transitive dependency closure, with the unit attaining
    // it. This is `maxFullEft` of docs/model.md 3.1, kept as a recurrence over direct dependencies
    // so that no closure is ever materialised.
    let mut closure_finish: Vec<Option<Candidate>> = vec![None; count];

    for &id in graph.topological_order() {
        let Some(unit) = graph.unit(id) else {
            continue;
        };
        let deps = graph.dependencies(id).unwrap_or(&[]);

        let mut closure: Option<Candidate> = None;
        for dep in deps {
            let index = dep.unit.index();
            let dep_finish = earliest_finish.get(index).copied().unwrap_or(Millis::ZERO);
            take_later(&mut closure, dep_finish, dep.unit);
            if let Some(inherited) = closure_finish.get(index).copied().flatten() {
                take_later(&mut closure, inherited.time, inherited.unit);
            }
        }
        if let Some(slot) = closure_finish.get_mut(id.index()) {
            *slot = closure;
        }

        let start = if unit.requires_full_closure {
            // Nothing this unit needs may still be running when it starts, so it waits for the
            // last finish anywhere behind it, whatever the intervening edges asked for.
            closure.map(|c| {
                (
                    c.time,
                    Reason {
                        producer: c.unit,
                        kind: EdgeKind::Full,
                    },
                )
            })
        } else {
            let mut best: Option<(Millis, Reason)> = None;
            for dep in deps {
                let Some(producer) = graph.unit(dep.unit) else {
                    continue;
                };
                let offset = release_offset(producer.total, producer.metadata_at, dep.kind);
                let dep_start = earliest_start
                    .get(dep.unit.index())
                    .copied()
                    .unwrap_or(Millis::ZERO);
                let release = dep_start.saturating_add(offset);
                let better = match best {
                    None => true,
                    Some((time, current)) => {
                        release > time || (release == time && dep.unit < current.producer)
                    }
                };
                if better {
                    best = Some((
                        release,
                        Reason {
                            producer: dep.unit,
                            kind: dep.kind,
                        },
                    ));
                }
            }
            best
        };

        let (start, why) = match start {
            Some((start, why)) => (start, Some(why)),
            None => (Millis::ZERO, None),
        };
        if let Some(slot) = earliest_start.get_mut(id.index()) {
            *slot = start;
        }
        if let Some(slot) = earliest_finish.get_mut(id.index()) {
            *slot = start.saturating_add(unit.total);
        }
        if let Some(slot) = reason.get_mut(id.index()) {
            *slot = why;
        }
    }

    (earliest_start, earliest_finish, reason)
}

/// The latest finish for every unit that does not lengthen the critical path.
///
/// The mirror of [`forward_pass`]: one visit per edge in reverse topological order. The closure
/// rule has to be applied here too, or a unit that a linker somewhere is waiting for would be
/// reported as movable.
fn backward_pass(
    graph: &BuildGraph,
    count: usize,
    critical_path_len: Millis,
    earliest_finish: &[Millis],
) -> Vec<Millis> {
    let mut latest_finish = vec![critical_path_len; count];
    // The earliest latest-start of any unit that requires this unit's closure, whether or not it is
    // a direct dependent. The mirror of `maxFullEft`, and derived the same way.
    let mut closure_cap: Vec<Option<Millis>> = vec![None; count];

    for &id in graph.topological_order().iter().rev() {
        let Some(unit) = graph.unit(id) else {
            continue;
        };

        let mut cap: Option<Millis> = None;
        let mut bound = critical_path_len;
        for dep in graph.dependents(id).unwrap_or(&[]) {
            let Some(consumer) = graph.unit(dep.unit) else {
                continue;
            };
            let consumer_latest_finish = latest_finish
                .get(dep.unit.index())
                .copied()
                .unwrap_or(critical_path_len);
            let consumer_latest_start = consumer_latest_finish.saturating_sub(consumer.total);

            if consumer.requires_full_closure {
                // This unit is in the consumer's closure, so it has to be finished before the
                // consumer starts. Anything requiring the consumer's closure requires this unit's
                // too, but its deadline is never the tighter one -- the consumer has to fit in
                // between -- so it does not have to be carried across separately.
                take_earlier(&mut cap, consumer_latest_start);
            } else {
                // A closure requirement further downstream reaches through this consumer.
                if let Some(inherited) = closure_cap.get(dep.unit.index()).copied().flatten() {
                    take_earlier(&mut cap, inherited);
                }
                let offset = release_offset(unit.total, unit.metadata_at, dep.kind);
                // The consumer waits for `offset` of this unit, so this unit may run on for the
                // remaining `total - offset` after that: a pipelined producer may finish after its
                // consumer has already started.
                let slack_past_release = unit.total.saturating_sub(offset);
                bound = bound.min(consumer_latest_start.saturating_add(slack_past_release));
            }
        }
        if let Some(cap) = cap {
            bound = bound.min(cap);
        }

        if let Some(slot) = closure_cap.get_mut(id.index()) {
            *slot = cap;
        }
        // A unit can always finish at its earliest possible time without delaying anything, so its
        // deadline is never earlier than that. The two coincide except when the critical path has
        // saturated, where subtracting from the clamped total would otherwise produce a deadline
        // that cannot be met.
        let floor = earliest_finish
            .get(id.index())
            .copied()
            .unwrap_or(Millis::ZERO);
        if let Some(slot) = latest_finish.get_mut(id.index()) {
            *slot = bound.max(floor);
        }
    }

    latest_finish
}

/// Traces the critical path back from the unit that finishes last, and prices each step.
fn recover_path(
    graph: &BuildGraph,
    count: usize,
    attaining: Option<UnitId>,
    reason: &[Option<Reason>],
) -> (Vec<UnitId>, Vec<Option<Millis>>) {
    let mut contribution: Vec<Option<Millis>> = vec![None; count];
    let mut path = Vec::new();

    let Some(attaining) = attaining else {
        return (path, contribution);
    };
    if let Some(unit) = graph.unit(attaining) {
        if let Some(slot) = contribution.get_mut(attaining.index()) {
            // Nothing on the path waits for the last unit, so all of it counts.
            *slot = Some(unit.total);
        }
    }

    let mut current = attaining;
    // Each step moves strictly backwards in topological order, so `count` steps cannot be exceeded;
    // the bound is here so that a defect elsewhere cannot turn into a hang.
    for _ in 0..count {
        path.push(current);
        let Some(why) = reason.get(current.index()).copied().flatten() else {
            break;
        };
        if let Some(producer) = graph.unit(why.producer) {
            if let Some(slot) = contribution.get_mut(why.producer.index()) {
                *slot = Some(release_offset(
                    producer.total,
                    producer.metadata_at,
                    why.kind,
                ));
            }
        }
        current = why.producer;
    }
    path.reverse();

    (path, contribution)
}

/// How much of a producer a consumer has to wait for.
///
/// A metadata edge from a unit that emits no metadata is rejected when the graph is built, so the
/// fallback is unreachable; it waits for the whole unit rather than assuming an offset of zero,
/// because guessing low here would understate every path through the unit.
fn release_offset(total: Millis, metadata_at: Option<Millis>, kind: EdgeKind) -> Millis {
    match kind {
        EdgeKind::Metadata => metadata_at.unwrap_or(total),
        EdgeKind::Full => total,
    }
}

/// A time and the unit responsible for it.
#[derive(Clone, Copy)]
struct Candidate {
    time: Millis,
    unit: UnitId,
}

/// Keeps the later time, and on a tie the lower identifier, so the choice never depends on the
/// order the candidates arrived in.
fn take_later(best: &mut Option<Candidate>, time: Millis, unit: UnitId) {
    let better = match best {
        None => true,
        Some(current) => time > current.time || (time == current.time && unit < current.unit),
    };
    if better {
        *best = Some(Candidate { time, unit });
    }
}

/// Keeps the earlier of two deadlines.
fn take_earlier(best: &mut Option<Millis>, time: Millis) {
    *best = Some(match *best {
        None => time,
        Some(current) => current.min(time),
    });
}

/// The critical path length and the lowest-numbered unit attaining it.
fn latest_finisher(graph: &BuildGraph, earliest_finish: &[Millis]) -> (Millis, Option<UnitId>) {
    let mut best: Option<Candidate> = None;
    for (id, _) in graph.units() {
        let finish = earliest_finish
            .get(id.index())
            .copied()
            .unwrap_or(Millis::ZERO);
        take_later(&mut best, finish, id);
    }
    match best {
        Some(best) => (best.time, Some(best.unit)),
        None => (Millis::ZERO, None),
    }
}
