//! Simulating a build on a bounded number of executors, and the bounds that hold whatever the
//! scheduler does.
//!
//! Section 5 of [`docs/model.md`](https://github.com/eventvoid/cargo-critpath/blob/main/docs/model.md) defines the simulator and section 6 the two bounds. The bounds are
//! guarantees; the simulated makespan is an estimate, because the ordering it reproduces is one
//! Cargo itself only follows up to hash iteration order.

use core::cmp::Reverse;
use core::num::{NonZeroU32, NonZeroU64};
use std::collections::BinaryHeap;

use crate::analysis::{analyze, Analysis};
use crate::graph::{BuildGraph, EdgeKind, UnitId};
use crate::millis::Millis;

/// Which unit a scheduler picks when several are ready and one executor is free.
///
/// More policies may be added, so callers must not assume the set is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Policy {
    /// Cargo's own ordering, which is blind to how long anything takes.
    ///
    /// A unit's priority is one plus the number of units that transitively depend on it. This is
    /// what Cargo does — it gives every unit the same fixed cost and adds up the costs of the
    /// transitive reverse-dependencies — and reproducing it is what makes the simulated makespan an
    /// estimate of Cargo's wall time rather than of a better build system's.
    CargoLike,
    /// The longest remaining path to a sink first, the standard critical-path heuristic.
    ///
    /// Schedules better than Cargo does. It is offered so that the gap between the two can be read
    /// off: that gap is what is lost to the ordering alone, as opposed to the shape of the graph.
    LongestPath,
}

impl Default for Policy {
    /// [`Policy::CargoLike`], because the question the tool is asked is how long *this* build
    /// takes, not how long an ideally scheduled one would.
    fn default() -> Self {
        Self::CargoLike
    }
}

/// When every unit ran, on a fixed number of executors under a fixed policy.
///
/// Produced by [`simulate`]. An identifier from a different graph is not detected; see [`Analysis`].
#[derive(Clone, Debug)]
pub struct Schedule {
    start: Vec<Millis>,
    finish: Vec<Millis>,
    makespan: Millis,
    policy: Policy,
    executors: NonZeroU32,
}

impl Schedule {
    /// When the last unit finished.
    ///
    /// Zero for an empty graph. This is an estimate, not a bound: see [`lower_bound`] for the
    /// numbers that hold whatever the scheduler does.
    #[must_use]
    pub fn makespan(&self) -> Millis {
        self.makespan
    }

    /// When a unit started, or `None` if the index is outside this schedule.
    #[must_use]
    pub fn start(&self, id: UnitId) -> Option<Millis> {
        self.start.get(id.index()).copied()
    }

    /// When a unit finished, or `None` if the index is outside this schedule.
    ///
    /// Always its start plus its whole duration: a unit is never preempted or migrated.
    #[must_use]
    pub fn finish(&self, id: UnitId) -> Option<Millis> {
        self.finish.get(id.index()).copied()
    }

    /// The policy this schedule was produced under.
    #[must_use]
    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// The executor count this schedule was produced for.
    #[must_use]
    pub fn executors(&self) -> NonZeroU32 {
        self.executors
    }
}

/// Simulates a build on `executors` executors.
///
/// A non-preemptive list scheduler: whenever an executor is free and some unit's dependencies have
/// released it, the highest-priority such unit starts and holds that executor for its whole
/// duration. Ties in priority go to the lower identifier, so the result never depends on iteration
/// order.
///
/// Costs `O((V + E) log V)` for [`Policy::LongestPath`], the logarithm being the ready queue.
/// [`Policy::CargoLike`] adds the cost of counting transitive dependents; see [`Policy`].
///
/// ```
/// use core::num::NonZeroU32;
/// use critpath_core::{simulate, EdgeKind, GraphBuilder, Millis, Policy, Unit, UnitKind};
///
/// let mut builder = GraphBuilder::new();
/// let mut add = |name: &str, total: u64| {
///     builder.add_unit(Unit {
///         package: name.to_owned(),
///         version: "1.0.0".to_owned(),
///         target: name.to_owned(),
///         kind: UnitKind::Lib,
///         total: Millis::new(total),
///         metadata_at: None,
///         requires_full_closure: false,
///     })
/// };
/// let slow = add("slow", 300)?;
/// let quick = add("quick", 100)?;
/// let last = add("last", 50)?;
/// builder.add_edge(quick, last, EdgeKind::Full)?;
/// let graph = builder.build()?;
///
/// // Two executors: slow runs 0..300, quick runs 0..100 and last follows it at 100..150.
/// let two = simulate(&graph, NonZeroU32::new(2).unwrap(), Policy::default());
/// assert_eq!(two.makespan(), Millis::new(300));
/// assert_eq!(two.start(last), Some(Millis::new(100)));
///
/// // One executor: quick outranks slow, having a dependent. At 100 both slow and last are ready
/// // and neither has a dependent, so the tie goes to the lower identifier: slow, then last.
/// let one = simulate(&graph, NonZeroU32::new(1).unwrap(), Policy::default());
/// assert_eq!(one.start(quick), Some(Millis::ZERO));
/// assert_eq!(one.start(slow), Some(Millis::new(100)));
/// assert_eq!(one.start(last), Some(Millis::new(400)));
/// assert_eq!(one.makespan(), Millis::new(450));
/// # Ok::<(), critpath_core::GraphError>(())
/// ```
#[must_use]
pub fn simulate(graph: &BuildGraph, executors: NonZeroU32, policy: Policy) -> Schedule {
    let count = graph.len();
    let priority = priorities(graph, policy);
    let mut state = Readiness::new(graph, count);
    let mut start = vec![Millis::ZERO; count];
    let mut finish = vec![Millis::ZERO; count];

    // Units whose release time is settled but has not arrived, earliest first.
    let mut waiting: Waiting = BinaryHeap::new();
    // Units that may start now, highest priority first and lowest identifier on a tie.
    let mut ready: BinaryHeap<(u64, Reverse<UnitId>)> = BinaryHeap::new();
    // Units in flight, earliest finish first.
    let mut running: BinaryHeap<Reverse<(Millis, UnitId)>> = BinaryHeap::new();

    for (id, _) in graph.units() {
        if graph.dependencies(id).unwrap_or(&[]).is_empty() {
            waiting.push(Reverse((Millis::ZERO, id)));
        }
    }

    let mut now = Millis::ZERO;
    let mut free = u64::from(executors.get());
    let mut makespan = Millis::ZERO;

    loop {
        let mut progress = false;

        // Retire everything that has finished by now, which frees executors and may release the
        // units waiting behind them.
        while let Some(&Reverse((at, id))) = running.peek() {
            if at > now {
                break;
            }
            running.pop();
            free = free.saturating_add(1);
            progress = true;
            state.on_finish(graph, id, at, &finish, &mut waiting);
        }

        // Move everything whose release time has arrived into the ready queue.
        while let Some(&Reverse((at, id))) = waiting.peek() {
            if at > now {
                break;
            }
            waiting.pop();
            let priority = priority.get(id.index()).copied().unwrap_or(0);
            ready.push((priority, Reverse(id)));
            progress = true;
        }

        // Fill the free executors.
        while free > 0 {
            let Some((_, Reverse(id))) = ready.pop() else {
                break;
            };
            let Some(unit) = graph.unit(id) else {
                continue;
            };
            let ends = now.saturating_add(unit.total);
            if let Some(slot) = start.get_mut(id.index()) {
                *slot = now;
            }
            if let Some(slot) = finish.get_mut(id.index()) {
                *slot = ends;
            }
            running.push(Reverse((ends, id)));
            free = free.saturating_sub(1);
            makespan = makespan.max(ends);
            progress = true;
            state.on_start(graph, id, now, &mut waiting);
        }

        if progress {
            continue;
        }

        // Nothing more can happen at this instant: jump to the next one that matters.
        let next_finish = running.peek().map(|&Reverse((at, _))| at);
        let next_release = waiting.peek().map(|&Reverse((at, _))| at);
        let next = match (next_finish, next_release) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        match next {
            Some(at) if at > now => now = at,
            // Either everything has run, or the queues hold only instants already passed, which
            // the retire and release steps above would have consumed.
            _ => break,
        }
    }

    Schedule {
        start,
        finish,
        makespan,
        policy,
        executors,
    }
}

/// The area bound: no schedule on `executors` executors can beat the total work divided between
/// them, rounded up because a makespan is a whole number of milliseconds.
///
/// ```
/// use core::num::NonZeroU32;
/// use critpath_core::{area_bound, GraphBuilder, Millis, Unit, UnitKind};
///
/// let mut builder = GraphBuilder::new();
/// for total in [20, 20, 21] {
///     builder.add_unit(Unit {
///         package: "p".to_owned(),
///         version: "1.0.0".to_owned(),
///         target: "p".to_owned(),
///         kind: UnitKind::Lib,
///         total: Millis::new(total),
///         metadata_at: None,
///         requires_full_closure: false,
///     })?;
/// }
/// let graph = builder.build()?;
///
/// // 61 ms of work on two executors cannot finish in 30 ms, so the bound is 31.
/// assert_eq!(area_bound(&graph, NonZeroU32::new(2).unwrap()), Millis::new(31));
/// # Ok::<(), critpath_core::GraphError>(())
/// ```
#[must_use]
pub fn area_bound(graph: &BuildGraph, executors: NonZeroU32) -> Millis {
    graph.total_work().div_ceil_by(NonZeroU64::from(executors))
}

/// The larger of the critical path and the area bound: no schedule on `executors` executors can
/// finish sooner, whatever policy it follows.
///
/// Which of the two terms dominates is the diagnosis. When the critical path does, the shape of the
/// graph is the constraint and more executors cannot help. When the area bound does, the machine is
/// the constraint and restructuring the graph will not buy much.
///
/// Both terms come from the one analysis, so there is no pair of arguments that can disagree about
/// which graph is being described.
#[must_use]
pub fn lower_bound(analysis: &Analysis, executors: NonZeroU32) -> Millis {
    let area = analysis
        .total_work()
        .div_ceil_by(NonZeroU64::from(executors));
    analysis.critical_path_len().max(area)
}

/// Units whose release time is known, ordered by that time and then by identifier.
type Waiting = BinaryHeap<Reverse<(Millis, UnitId)>>;

/// Tracks what still has to happen before each unit may start.
///
/// Two independent channels, because the model has two kinds of constraint. Ordinary units count
/// down their incoming edges, each satisfied when the producer starts (a metadata edge) or finishes
/// (a full edge). Units that require their whole closure ignore their edges and wait on the second
/// channel, which counts down direct dependencies that are themselves *settled* — finished, with
/// everything behind them settled too. That makes the last finish anywhere in a closure available
/// without ever enumerating the closure, in `O(V + E)` over the whole run.
struct Readiness {
    /// Incoming edges not yet satisfied, for units that do not require their closure.
    unmet_edges: Vec<u32>,
    /// When the last satisfied incoming edge releases the unit.
    released_at: Vec<Millis>,
    /// Direct dependencies not yet settled.
    unsettled_deps: Vec<u32>,
    /// The last finish anywhere behind a unit, final once `unsettled_deps` reaches zero.
    closure_finish: Vec<Millis>,
    finished: Vec<bool>,
}

impl Readiness {
    fn new(graph: &BuildGraph, count: usize) -> Self {
        let degrees: Vec<u32> = graph
            .units()
            .map(|(id, _)| {
                let deps = graph.dependencies(id).unwrap_or(&[]).len();
                u32::try_from(deps).unwrap_or(u32::MAX)
            })
            .collect();
        Self {
            unmet_edges: degrees.clone(),
            released_at: vec![Millis::ZERO; count],
            unsettled_deps: degrees,
            closure_finish: vec![Millis::ZERO; count],
            finished: vec![false; count],
        }
    }

    /// A unit has started: its metadata is now on a known schedule.
    fn on_start(&mut self, graph: &BuildGraph, id: UnitId, at: Millis, waiting: &mut Waiting) {
        let Some(unit) = graph.unit(id) else {
            return;
        };
        for dep in graph.dependents(id).unwrap_or(&[]) {
            if dep.kind != EdgeKind::Metadata || requires_closure(graph, dep.unit) {
                continue;
            }
            // A metadata edge from a unit that emits none is rejected when the graph is built; the
            // fallback waits for the whole unit rather than releasing the consumer too early.
            let offset = unit.metadata_at.unwrap_or(unit.total);
            self.satisfy_edge(dep.unit, at.saturating_add(offset), waiting);
        }
    }

    /// A unit has finished: full edges out of it are satisfied, and it may now settle.
    fn on_finish(
        &mut self,
        graph: &BuildGraph,
        id: UnitId,
        at: Millis,
        finish: &[Millis],
        waiting: &mut Waiting,
    ) {
        if let Some(slot) = self.finished.get_mut(id.index()) {
            *slot = true;
        }
        for dep in graph.dependents(id).unwrap_or(&[]) {
            if dep.kind != EdgeKind::Full || requires_closure(graph, dep.unit) {
                continue;
            }
            self.satisfy_edge(dep.unit, at, waiting);
        }
        self.settle(graph, id, finish, waiting);
    }

    /// Records one satisfied incoming edge, and queues the consumer once all of them are.
    fn satisfy_edge(&mut self, consumer: UnitId, release: Millis, waiting: &mut Waiting) {
        if let Some(slot) = self.released_at.get_mut(consumer.index()) {
            *slot = (*slot).max(release);
        }
        let Some(remaining) = self.unmet_edges.get_mut(consumer.index()) else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            let at = self
                .released_at
                .get(consumer.index())
                .copied()
                .unwrap_or(Millis::ZERO);
            waiting.push(Reverse((at, consumer)));
        }
    }

    /// Propagates "everything behind this unit is done, and so is the unit itself".
    ///
    /// Written as a worklist rather than as recursion: a chain of pipelined libraries settles from
    /// the front all at once, and recursion would make its depth the stack depth.
    fn settle(&mut self, graph: &BuildGraph, id: UnitId, finish: &[Millis], waiting: &mut Waiting) {
        let mut worklist = Vec::new();
        if self.is_settled(id) {
            worklist.push(id);
        }
        while let Some(settled) = worklist.pop() {
            let behind = finish
                .get(settled.index())
                .copied()
                .unwrap_or(Millis::ZERO)
                .max(
                    self.closure_finish
                        .get(settled.index())
                        .copied()
                        .unwrap_or(Millis::ZERO),
                );
            for dep in graph.dependents(settled).unwrap_or(&[]) {
                if let Some(slot) = self.closure_finish.get_mut(dep.unit.index()) {
                    *slot = (*slot).max(behind);
                }
                let Some(remaining) = self.unsettled_deps.get_mut(dep.unit.index()) else {
                    continue;
                };
                *remaining = remaining.saturating_sub(1);
                if *remaining > 0 {
                    continue;
                }
                if requires_closure(graph, dep.unit) {
                    let at = self
                        .closure_finish
                        .get(dep.unit.index())
                        .copied()
                        .unwrap_or(Millis::ZERO);
                    waiting.push(Reverse((at, dep.unit)));
                }
                if self.is_settled(dep.unit) {
                    worklist.push(dep.unit);
                }
            }
        }
    }

    /// Whether a unit has finished and everything behind it has settled.
    fn is_settled(&self, id: UnitId) -> bool {
        self.finished.get(id.index()).copied().unwrap_or(false)
            && self.unsettled_deps.get(id.index()).copied().unwrap_or(0) == 0
    }
}

fn requires_closure(graph: &BuildGraph, id: UnitId) -> bool {
    graph
        .unit(id)
        .is_some_and(|unit| unit.requires_full_closure)
}

/// The scheduling priority of every unit, higher first.
fn priorities(graph: &BuildGraph, policy: Policy) -> Vec<u64> {
    match policy {
        Policy::CargoLike => transitive_dependents(graph)
            .into_iter()
            .map(|count| count.saturating_add(1))
            .collect(),
        Policy::LongestPath => {
            let analysis = analyze(graph);
            let critical_path = analysis.critical_path_len();
            graph
                .units()
                .map(|(id, unit)| {
                    // The longest path from this unit's start to the end of the build. A unit that
                    // may finish as late as `latest_finish` without lengthening the build has
                    // exactly that much less path behind it.
                    let latest_start = analysis
                        .latest_finish(id)
                        .unwrap_or(critical_path)
                        .saturating_sub(unit.total);
                    critical_path.saturating_sub(latest_start).get()
                })
                .collect()
        }
    }
}

/// How many distinct units transitively depend on each unit.
///
/// Reachability counting, which no linear-time algorithm computes on a general DAG. This walks the
/// graph once per block of 64 units, propagating a 64-bit membership mask backwards through the
/// topological order, for `O((V / 64) * (V + E))` time in `O(V)` memory. The alternative, one
/// bitset per unit, is a single pass but needs `V * V` bits, which a workspace of tens of thousands
/// of units would not survive.
fn transitive_dependents(graph: &BuildGraph) -> Vec<u64> {
    let count = graph.len();
    let mut totals = vec![0u64; count];
    let mut reachable = vec![0u64; count];
    let order = graph.topological_order();

    let mut block = 0usize;
    while block < count {
        let end = block.saturating_add(64).min(count);
        reachable.fill(0);
        for &id in order.iter().rev() {
            let mut mask = 0u64;
            for dep in graph.dependents(id).unwrap_or(&[]) {
                let index = dep.unit.index();
                if index >= block && index < end {
                    mask |= 1u64 << (index - block);
                }
                mask |= reachable.get(index).copied().unwrap_or(0);
            }
            if let Some(slot) = reachable.get_mut(id.index()) {
                *slot = mask;
            }
        }
        for (index, mask) in reachable.iter().enumerate() {
            if let Some(total) = totals.get_mut(index) {
                *total = total.saturating_add(u64::from(mask.count_ones()));
            }
        }
        block = block.saturating_add(64);
    }

    totals
}
