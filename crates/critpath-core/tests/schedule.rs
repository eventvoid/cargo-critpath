//! The bounded-executor simulation, its two policies, and the bounds that hold for both.
//!
//! Hand-computed cases write the trace out in a comment: which unit starts at which instant and
//! why. The property checks replay the produced schedule against the graph rather than against a
//! second copy of the scheduler.
//!
//! Invariant numbers cited here are those of `docs/model.md` section 7.

// A test reports failure by panicking, and unwrapping a value the test has just asserted is
// present is clearer than propagating an error no caller can act on. The division is the area
// bound, recomputed here in the long form so that the expected value does not come from the
// helper that is under test.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division
)]

use std::collections::VecDeque;
use std::num::NonZeroU32;

use critpath_core::{
    analyze, area_bound, lower_bound, simulate, BuildGraph, EdgeKind, GraphBuilder, Millis, Policy,
    Unit, UnitId, UnitKind,
};

/// How the shape tables below describe a unit: total, metadata offset, closure requirement.
type Spec = (u64, Option<u64>, bool);
/// How the shape tables below describe an edge: producer index, consumer index, kind.
type EdgeSpec = (usize, usize, EdgeKind);

const POLICIES: [Policy; 2] = [Policy::CargoLike, Policy::LongestPath];

fn j(executors: u32) -> NonZeroU32 {
    NonZeroU32::new(executors).unwrap()
}

fn unit_of(name: &str, (total, metadata, closure): Spec) -> Unit {
    Unit {
        package: name.to_owned(),
        version: "1.0.0".to_owned(),
        target: name.to_owned(),
        kind: if closure {
            UnitKind::Bin
        } else {
            UnitKind::Lib
        },
        total: Millis::new(total),
        metadata_at: metadata.map(Millis::new),
        requires_full_closure: closure,
    }
}

fn build(specs: &[Spec], edges: &[EdgeSpec]) -> (BuildGraph, Vec<UnitId>) {
    let mut builder = GraphBuilder::new();
    let ids: Vec<UnitId> = specs
        .iter()
        .enumerate()
        .map(|(i, spec)| builder.add_unit(unit_of(&format!("u{i}"), *spec)).unwrap())
        .collect();
    for &(from, to, kind) in edges {
        builder.add_edge(ids[from], ids[to], kind).unwrap();
    }
    (builder.build().unwrap(), ids)
}

/// The units `id` transitively depends on, enumerated explicitly, so that the closure rule can be
/// checked against the schedule without reusing the recurrence that produced it.
fn enumerated_closure(graph: &BuildGraph, id: UnitId) -> Vec<UnitId> {
    let mut seen = vec![false; graph.len()];
    let mut queue: VecDeque<UnitId> = VecDeque::new();
    let mut out = Vec::new();
    queue.push_back(id);
    seen[id.index()] = true;
    while let Some(current) = queue.pop_front() {
        for dep in graph.dependencies(current).unwrap() {
            if !seen[dep.unit.index()] {
                seen[dep.unit.index()] = true;
                out.push(dep.unit);
                queue.push_back(dep.unit);
            }
        }
    }
    out.sort();
    out
}

/// The graphs the property checks run over. Named so a failure says which one broke.
fn shapes() -> Vec<(&'static str, Vec<Spec>, Vec<EdgeSpec>)> {
    vec![
        (
            "chain of full edges",
            (0..5).map(|_| (70, None, false)).collect(),
            (0..4).map(|i| (i, i + 1, EdgeKind::Full)).collect(),
        ),
        (
            "proc-macro over a chain, the measured fixture",
            vec![
                (1240, Some(780), false),
                (60, Some(20), false),
                (210, None, true),
                (30, Some(30), false),
            ],
            vec![
                (0, 1, EdgeKind::Metadata),
                (1, 2, EdgeKind::Full),
                (1, 3, EdgeKind::Metadata),
            ],
        ),
        (
            "wide fan-out from one source",
            vec![
                (100, Some(20), false),
                (40, None, false),
                (90, None, false),
                (25, None, false),
                (10, None, false),
                (60, None, false),
                (5, None, false),
            ],
            (1..7).map(|i| (0, i, EdgeKind::Metadata)).collect(),
        ),
        (
            "two closure units stacked over a wide layer",
            vec![
                (500, Some(100), false),
                (40, Some(10), false),
                (70, Some(60), false),
                (20, None, true),
                (90, Some(5), false),
                (15, None, true),
            ],
            vec![
                (0, 1, EdgeKind::Metadata),
                (0, 2, EdgeKind::Metadata),
                (1, 3, EdgeKind::Full),
                (2, 3, EdgeKind::Full),
                (1, 4, EdgeKind::Metadata),
                (3, 5, EdgeKind::Full),
                (4, 5, EdgeKind::Metadata),
            ],
        ),
        (
            "a decoy with many dependents beside a heavy chain",
            decoy_specs(),
            decoy_edges(),
        ),
        (
            "a late metadata release",
            vec![(100, Some(10), false), (50, None, false)],
            vec![(0, 1, EdgeKind::Metadata)],
        ),
        (
            "zero weights and a zero metadata offset",
            vec![(0, Some(0), false), (0, Some(0), false), (5, None, true)],
            vec![(0, 1, EdgeKind::Metadata), (1, 2, EdgeKind::Full)],
        ),
        (
            "disconnected components",
            vec![
                (10, Some(4), false),
                (20, None, false),
                (30, Some(30), false),
                (7, None, true),
            ],
            vec![(2, 3, EdgeKind::Full)],
        ),
    ]
}

/// A chain of two heavy units beside two decoys that have three trivial dependents each.
///
/// The decoys carry more structural weight than the chain and less real work, which is exactly the
/// case where reproducing Cargo's cost-blind ordering and choosing a good ordering diverge.
fn decoy_specs() -> Vec<Spec> {
    let mut specs = vec![
        (40, None, false), // 0: chain head
        (40, None, false), // 1: chain tail
        (50, None, false), // 2: first decoy
        (50, None, false), // 3: second decoy
    ];
    specs.extend((0..6).map(|_| (1, None, false))); // 4..=9: the decoys' dependents
    specs
}

fn decoy_edges() -> Vec<EdgeSpec> {
    let mut edges = vec![(0, 1, EdgeKind::Full)];
    edges.extend((4..7).map(|i| (2, i, EdgeKind::Full)));
    edges.extend((7..10).map(|i| (3, i, EdgeKind::Full)));
    edges
}

#[test]
fn i12_empty_graph_schedules_to_nothing() {
    let (single, ids) = build(&[(500, None, false)], &[]);
    let foreign = ids[0];
    let empty = GraphBuilder::new().build().unwrap();
    assert_eq!(single.len(), 1);

    for executors in [1, 2, 64] {
        let schedule = simulate(&empty, j(executors), Policy::CargoLike);
        assert_eq!(schedule.makespan(), Millis::ZERO);
        // No unit exists, and an identifier from another graph is not read as unit zero.
        assert_eq!(schedule.start(foreign), None);
        assert_eq!(schedule.finish(foreign), None);
        // No division by zero, and no bound that a schedule of nothing cannot meet.
        assert_eq!(area_bound(&empty, j(executors)), Millis::ZERO);
        assert_eq!(
            lower_bound(&empty, &analyze(&empty), j(executors)),
            Millis::ZERO
        );
    }
}

#[test]
fn i11_a_single_unit_takes_its_own_time_on_any_number_of_executors() {
    let (graph, ids) = build(&[(500, Some(300), false)], &[]);
    for executors in [1, 2, 5] {
        for policy in POLICIES {
            let schedule = simulate(&graph, j(executors), policy);
            assert_eq!(schedule.makespan(), Millis::new(500));
            assert_eq!(schedule.start(ids[0]), Some(Millis::ZERO));
            assert_eq!(schedule.finish(ids[0]), Some(Millis::new(500)));
        }
    }
}

#[test]
fn i7_enough_executors_reach_the_critical_path_exactly() {
    for (name, specs, edges) in shapes() {
        let (graph, _) = build(&specs, &edges);
        let critical_path = analyze(&graph).critical_path_len();
        // One executor per unit means nothing ever queues, so the only thing left holding a unit
        // back is the graph, and the makespan must be the critical path to the millisecond.
        for executors in [graph.len(), graph.len() + 3] {
            let executors = j(u32::try_from(executors).unwrap());
            for policy in POLICIES {
                assert_eq!(
                    simulate(&graph, executors, policy).makespan(),
                    critical_path,
                    "{name}: {policy:?} on {executors} executors"
                );
            }
        }
    }
}

#[test]
fn i5_and_i6_every_schedule_meets_both_bounds() {
    for (name, specs, edges) in shapes() {
        let (graph, _) = build(&specs, &edges);
        let analysis = analyze(&graph);
        let work = graph.total_work().get();

        for executors in 1..=(graph.len() as u32 + 2) {
            let area = area_bound(&graph, j(executors));
            // Computed independently of the implementation: the ceiling of the division, because a
            // makespan is a whole number of milliseconds and a bound that rounded down would be
            // one no schedule could reach.
            let expected_area =
                work / u64::from(executors) + u64::from(work % u64::from(executors) != 0);
            assert_eq!(area, Millis::new(expected_area), "{name}: area bound");

            let bound = lower_bound(&graph, &analysis, j(executors));
            assert_eq!(
                bound,
                analysis.critical_path_len().max(area),
                "{name}: lower bound"
            );

            for policy in POLICIES {
                let makespan = simulate(&graph, j(executors), policy).makespan();
                assert!(
                    makespan >= analysis.critical_path_len(),
                    "{name}: {policy:?} on {executors} beat the critical path"
                );
                assert!(
                    makespan >= area,
                    "{name}: {policy:?} on {executors} beat the area bound"
                );
                assert!(makespan >= bound, "{name}: {policy:?} on {executors}");
            }
        }
    }
}

#[test]
fn i2_and_i4_the_schedule_respects_every_constraint_it_was_given() {
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        for executors in 1..=(graph.len() as u32 + 1) {
            for policy in POLICIES {
                let schedule = simulate(&graph, j(executors), policy);
                let where_ = format!("{name}: {policy:?} on {executors}");

                for id in &ids {
                    let unit = graph.unit(*id).unwrap();
                    let start = schedule.start(*id).unwrap();
                    // I4: one executor for exactly `total`, so the finish is not an independent
                    // number that could drift from the start.
                    assert_eq!(
                        schedule.finish(*id),
                        Some(start.saturating_add(unit.total)),
                        "{where_}: unit {id} finish"
                    );
                    assert!(
                        schedule.finish(*id).unwrap() <= schedule.makespan(),
                        "{where_}: unit {id} finishes after the makespan"
                    );

                    // I2: for every incoming edge, the release time has passed.
                    for dep in graph.dependencies(*id).unwrap() {
                        let producer = graph.unit(dep.unit).unwrap();
                        let offset = match dep.kind {
                            EdgeKind::Metadata => producer.metadata_at.unwrap(),
                            EdgeKind::Full => producer.total,
                        };
                        let release = schedule.start(dep.unit).unwrap().saturating_add(offset);
                        assert!(
                            start >= release,
                            "{where_}: unit {id} started at {start} before {} released it at \
                             {release}",
                            dep.unit
                        );
                    }

                    // I5 applied inside the simulation: the closure rule holds here too, not only
                    // in the analysis. Enumerated explicitly rather than by the recurrence.
                    if unit.requires_full_closure {
                        for behind in enumerated_closure(&graph, *id) {
                            assert!(
                                start >= schedule.finish(behind).unwrap(),
                                "{where_}: unit {id} started at {start} while {behind} in its \
                                 closure was still running"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn i3_never_more_units_running_than_executors() {
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        for executors in 1..=(graph.len() as u32 + 1) {
            for policy in POLICIES {
                let schedule = simulate(&graph, j(executors), policy);
                let spans: Vec<(u64, u64)> = ids
                    .iter()
                    .map(|id| {
                        (
                            schedule.start(*id).unwrap().get(),
                            schedule.finish(*id).unwrap().get(),
                        )
                    })
                    .collect();

                // Occupancy can only rise at an instant some unit starts, so checking those
                // instants checks every instant. A unit of zero length occupies nothing, which is
                // what the half-open interval of I4 says.
                for &(instant, _) in &spans {
                    let running = spans
                        .iter()
                        .filter(|(start, end)| *start <= instant && instant < *end)
                        .count();
                    assert!(
                        running <= executors as usize,
                        "{name}: {policy:?} ran {running} units at {instant} on {executors} \
                         executors"
                    );
                }
            }
        }
    }
}

#[test]
fn i8_more_executors_never_lengthen_these_schedules() {
    // I8 states that makespan(J + 1) <= makespan(J) always. It does not: see
    // `more_executors_can_lengthen_a_schedule` below for a graph where it fails, and the reason it
    // fails. What is checked here is the weaker true statement, that these shapes are monotone,
    // which is worth pinning down because the anomaly is rare enough that a regression making it
    // common would otherwise pass unnoticed.
    for (name, specs, edges) in shapes() {
        let (graph, _) = build(&specs, &edges);
        for policy in POLICIES {
            let mut previous = None;
            for executors in 1..=(graph.len() as u32 + 2) {
                let makespan = simulate(&graph, j(executors), policy).makespan();
                if let Some(previous) = previous {
                    assert!(
                        makespan <= previous,
                        "{name}: {policy:?} took {makespan} on {executors} executors after \
                         {previous} on one fewer"
                    );
                }
                previous = Some(makespan);
            }
        }
    }
}

#[test]
fn more_executors_can_lengthen_a_schedule() {
    // Seven units. u3 (3 ms, metadata at 2) releases u4 (3 ms), which releases u6 (2 ms); the
    // other four are independent filler of 1, 1, 2 and 2 ms. Cargo-like priorities are structural,
    // so u3 is 3, u4 is 2 and everything else is 1.
    //
    // On two executors:
    //   0  u3 0..3 and u0 0..1     1  u1 1..2
    //   2  u3 emits its metadata and u4, at priority 2, takes the executor u1 just freed: 2..5
    //   3  u2 3..5                 5  u5 5..7 and u6 5..7          makespan 7
    //
    // On three executors:
    //   0  u3 0..3, u0 0..1, u1 0..1
    //   1  two executors free, and u4 is not released until 2, so the filler takes them both:
    //      u2 1..3 and u5 1..3
    //   2  u4 is released, and all three executors are busy until 3
    //   3  u4 3..6                 6  u6 6..8                      makespan 8
    //
    // The extra executor let the filler start sooner, and the filler was still holding the machines
    // at the instant the unit on the critical path became runnable. This is a Graham anomaly: a
    // greedy list scheduler is not monotone in the number of machines. It is rare rather than
    // pathological -- a search over 200 000 random graphs of three to eight units found it in 8 of
    // them -- and it is not caused by the metadata semantics, since the same search restricted to
    // full edges and no metadata found 8 in 1 000 000 graphs.
    //
    // The critical path is 7, which is what makes the three-executor answer visibly wrong: it is
    // above the bound its own graph guarantees on two.
    let (graph, ids) = build(
        &[
            (1, None, false),
            (1, None, false),
            (2, None, false),
            (3, Some(2), false),
            (3, None, false),
            (2, None, false),
            (2, None, false),
        ],
        &[(3, 4, EdgeKind::Metadata), (4, 6, EdgeKind::Full)],
    );

    let two = simulate(&graph, j(2), Policy::CargoLike);
    let three = simulate(&graph, j(3), Policy::CargoLike);

    assert_eq!(two.makespan(), Millis::new(7));
    assert_eq!(three.makespan(), Millis::new(8));
    assert!(three.makespan() > two.makespan());

    assert_eq!(two.start(ids[4]), Some(Millis::new(2)));
    assert_eq!(three.start(ids[4]), Some(Millis::new(3)));
    assert_eq!(three.start(ids[2]), Some(Millis::new(1)));
    assert_eq!(three.start(ids[5]), Some(Millis::new(1)));

    // Nothing else gives way: the critical path is 7 on both, and both schedules still meet it.
    let analysis = analyze(&graph);
    assert_eq!(analysis.critical_path_len(), Millis::new(7));
    assert!(two.makespan() >= analysis.critical_path_len());
    assert!(three.makespan() >= analysis.critical_path_len());

    // Beyond the anomaly the curve recovers, so the extra executors are not simply wasted.
    assert_eq!(
        simulate(&graph, j(4), Policy::CargoLike).makespan(),
        Millis::new(7)
    );
}

#[test]
fn i1_repeated_simulation_of_the_same_graph_agrees_exactly() {
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        for policy in POLICIES {
            let first = simulate(&graph, j(3), policy);
            let second = simulate(&graph, j(3), policy);
            assert_eq!(first.makespan(), second.makespan(), "{name}");
            for id in &ids {
                assert_eq!(first.start(*id), second.start(*id), "{name}: unit {id}");
            }
        }
    }
}

#[test]
fn cargo_like_ordering_counts_transitive_dependents_not_direct_ones() {
    // A(10) heads a chain of five trivial units; B(10) has two trivial dependents and nothing
    // beyond them. B has more direct dependents than A, A has more transitive ones:
    //
    //   priority = 1 + transitive dependents:  A 6, a1 5, a2 4, a3 3, a4 2, a5 1, B 3, b1 1, b2 1
    //   a direct count would have given:       A 2                                B 3
    //
    // On one executor the order is exactly the priority order over whatever is ready, so the start
    // times spell the ordering out:
    //
    //   0  A      ready {A 6, B 3}                 10 a1  ready {a1 5, B 3}
    //   11 a2     ready {a2 4, B 3}                12 a3  ready {a3 3, B 3}, tie broken by id 3 < 6
    //   13 B      ready {a4 2, B 3}                23 a4  ready {a4 2, b1 1, b2 1}
    //   24 a5     tie at 1, id 5 < 7               25 b1  26 b2  -> ends at 27
    let mut specs = vec![(10, None, false)];
    specs.extend((0..5).map(|_| (1, None, false)));
    specs.push((10, None, false));
    specs.extend((0..2).map(|_| (1, None, false)));
    let mut edges: Vec<EdgeSpec> = (0..5).map(|i| (i, i + 1, EdgeKind::Full)).collect();
    edges.push((6, 7, EdgeKind::Full));
    edges.push((6, 8, EdgeKind::Full));

    let (graph, ids) = build(&specs, &edges);
    let schedule = simulate(&graph, j(1), Policy::CargoLike);

    let starts: Vec<u64> = ids
        .iter()
        .map(|id| schedule.start(*id).unwrap().get())
        .collect();
    assert_eq!(starts, vec![0, 10, 11, 12, 23, 24, 13, 25, 26]);
    // One executor can never idle here, so the makespan is the total work: 10 + 5 + 10 + 2.
    assert_eq!(schedule.makespan(), Millis::new(27));
    assert_eq!(graph.total_work(), Millis::new(27));
}

#[test]
fn the_two_policies_can_differ_by_a_third_of_the_build() {
    // Two heavy units in a chain, and two decoys that each have three trivial dependents. On two
    // executors:
    //
    // cargo-like priorities are structural: head 2, tail 1, decoys 4 each, dependents 1.
    //   0   both decoys start, being the two highest; the chain head waits
    //   50  decoys done. head (2) beats the trivial units (1) -> head 50..90, then u4 50..51,
    //       u5 51..52, u6 52..53, u7 53..54, u8 54..55, u9 55..56 on the other executor
    //   90  head done, tail 90..130                                   makespan 130
    //
    // longest-path priorities are the remaining path: head 80, tail 40, decoys 51, trivial 1.
    //   0   head 0..40 and first decoy 0..50
    //   40  head done; second decoy (51) beats the tail (40) -> decoy 40..90
    //   50  first decoy done, tail 50..90
    //   90  both done; six trivial units on two executors: 90..91 twice, 91..92 twice,
    //       92..93 twice                                              makespan 93
    //
    // Total work is 40 + 40 + 50 + 50 + 6 = 186, so on two executors nothing can finish before
    // ceil(186 / 2) = 93: the longest-path schedule is optimal and cargo-like loses 37 ms to its
    // ordering alone.
    let (graph, ids) = build(&decoy_specs(), &decoy_edges());
    assert_eq!(graph.total_work(), Millis::new(186));

    let cargo = simulate(&graph, j(2), Policy::CargoLike);
    let longest = simulate(&graph, j(2), Policy::LongestPath);

    assert_eq!(cargo.makespan(), Millis::new(130));
    assert_eq!(longest.makespan(), Millis::new(93));
    assert_eq!(area_bound(&graph, j(2)), Millis::new(93));
    assert_eq!(cargo.start(ids[0]), Some(Millis::new(50)));
    assert_eq!(longest.start(ids[0]), Some(Millis::ZERO));

    // Each schedule reports the parameters it was produced with, so output can name them.
    assert_eq!(cargo.policy(), Policy::CargoLike);
    assert_eq!(longest.policy(), Policy::LongestPath);
    assert_eq!(cargo.executors(), j(2));

    // Neither beats the critical path of 80, and the default is the one that reproduces Cargo.
    assert_eq!(analyze(&graph).critical_path_len(), Millis::new(80));
    assert_eq!(Policy::default(), Policy::CargoLike);
}

#[test]
fn a_metadata_release_in_the_future_leaves_an_executor_idle() {
    // u0 (100 ms, metadata at 10) -> u1 (50 ms) over a metadata edge, on two executors. u1 cannot
    // start at 0 even though an executor is free: nothing releases it until 10.
    //   est(u1) = 10, so u1 runs 10..60 and u0 runs 0..100. makespan = 100 = CP.
    let (graph, ids) = build(
        &[(100, Some(10), false), (50, None, false)],
        &[(0, 1, EdgeKind::Metadata)],
    );

    for policy in POLICIES {
        let schedule = simulate(&graph, j(2), policy);
        assert_eq!(schedule.start(ids[0]), Some(Millis::ZERO));
        assert_eq!(schedule.start(ids[1]), Some(Millis::new(10)));
        assert_eq!(schedule.makespan(), Millis::new(100));
    }

    // On one executor the release is moot: u0 holds the executor to 100, u1 follows.
    let single = simulate(&graph, j(1), Policy::CargoLike);
    assert_eq!(single.start(ids[1]), Some(Millis::new(100)));
    assert_eq!(single.makespan(), Millis::new(150));
    assert_eq!(area_bound(&graph, j(1)), Millis::new(150));
}

#[test]
fn the_area_bound_rounds_up_and_the_lower_bound_takes_whichever_term_dominates() {
    // Three independent units of 20, 20 and 21 ms: total work 61, critical path 21.
    let (graph, _) = build(
        &[(20, None, false), (20, None, false), (21, None, false)],
        &[],
    );
    let analysis = analyze(&graph);
    assert_eq!(graph.total_work(), Millis::new(61));
    assert_eq!(analysis.critical_path_len(), Millis::new(21));

    // 61 / 2 is 30.5, and no schedule on two executors can finish in 30, so the bound is 31.
    assert_eq!(area_bound(&graph, j(2)), Millis::new(31));
    assert_eq!(lower_bound(&graph, &analysis, j(2)), Millis::new(31));
    // The machine is the constraint here, and the bound is not attainable either: two executors
    // run 20, 20 in parallel and then 21 alone, ending at 41.
    assert_eq!(
        simulate(&graph, j(2), Policy::CargoLike).makespan(),
        Millis::new(41)
    );

    // One executor: all the work is serial, and the area term dominates completely.
    assert_eq!(area_bound(&graph, j(1)), Millis::new(61));
    assert_eq!(lower_bound(&graph, &analysis, j(1)), Millis::new(61));

    // Four executors: 61 / 4 rounds up to 16, below the critical path, so the graph is the
    // constraint and the lower bound is the critical path.
    assert_eq!(area_bound(&graph, j(4)), Millis::new(16));
    assert_eq!(lower_bound(&graph, &analysis, j(4)), Millis::new(21));
}
