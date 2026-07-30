//! Earliest and latest times, the critical path, slack, and the closure rule.
//!
//! Every expected value is computed by hand in the test body, with the arithmetic written out in a
//! comment. Where a whole shape is checked at once, it is checked against a deliberately naive
//! reference implemented in this file, never against the code under test.
//!
//! Invariant numbers cited here are those of `docs/model.md` section 7.

// A test reports failure by panicking, and unwrapping a value the test has just asserted is
// present is clearer than propagating an error no caller can act on.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::VecDeque;

use critpath_core::{analyze, BuildGraph, EdgeKind, GraphBuilder, Millis, Unit, UnitId, UnitKind};

/// How the shape tables below describe a unit: total, metadata offset, closure requirement.
type Spec = (u64, Option<u64>, bool);
/// How the shape tables below describe an edge: producer index, consumer index, kind.
type EdgeSpec = (usize, usize, EdgeKind);

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

/// The units `id` transitively depends on, enumerated explicitly by breadth-first search.
///
/// This is the closure the model deliberately never materialises. It exists here so that the
/// recurrence can be checked against it (I18).
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

/// A deliberately naive reference for earliest start and finish times.
///
/// It relaxes every unit in identifier order, `n + 1` times, which converges because the values
/// only ever increase and a DAG of `n` units has depth at most `n`. Closure requirements are
/// enforced by enumerating each closure explicitly. It is `O(V * E)` or worse and shares no code
/// with the implementation, which is the point: it is the oracle, not a second copy of the
/// algorithm.
fn naive_times(graph: &BuildGraph) -> (Vec<u64>, Vec<u64>) {
    let n = graph.len();
    let mut est = vec![0u64; n];
    let mut eft = vec![0u64; n];
    for _ in 0..=n {
        for (id, unit) in graph.units() {
            let start = if unit.requires_full_closure {
                enumerated_closure(graph, id)
                    .iter()
                    .map(|d| eft[d.index()])
                    .max()
                    .unwrap_or(0)
            } else {
                graph
                    .dependencies(id)
                    .unwrap()
                    .iter()
                    .map(|dep| {
                        let producer = graph.unit(dep.unit).unwrap();
                        let offset = match dep.kind {
                            EdgeKind::Metadata => producer.metadata_at.unwrap().get(),
                            EdgeKind::Full => producer.total.get(),
                        };
                        est[dep.unit.index()] + offset
                    })
                    .max()
                    .unwrap_or(0)
            };
            est[id.index()] = start;
            eft[id.index()] = start + unit.total.get();
        }
    }
    (est, eft)
}

/// The shapes the whole-graph checks run over. Named so a failure says which one broke.
fn shapes() -> Vec<(&'static str, Vec<Spec>, Vec<EdgeSpec>)> {
    vec![
        (
            "chain of metadata edges",
            vec![
                (100, Some(30), false),
                (60, Some(20), false),
                (80, Some(80), false),
            ],
            vec![(0, 1, EdgeKind::Metadata), (1, 2, EdgeKind::Metadata)],
        ),
        (
            "diamond, metadata in front and full behind",
            vec![
                (100, Some(40), false),
                (50, Some(20), false),
                (80, Some(30), false),
                (60, None, false),
            ],
            vec![
                (0, 1, EdgeKind::Metadata),
                (0, 2, EdgeKind::Metadata),
                (1, 3, EdgeKind::Full),
                (2, 3, EdgeKind::Full),
            ],
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
            "a closure unit that itself emits metadata",
            vec![
                (100, Some(50), false),
                (80, Some(40), true),
                (60, Some(10), false),
            ],
            vec![(0, 1, EdgeKind::Metadata), (1, 2, EdgeKind::Metadata)],
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

#[test]
fn i12_empty_graph_has_no_critical_path() {
    let (single, ids) = build(&[(500, Some(300), false)], &[]);
    let foreign = ids[0];
    assert_eq!(single.len(), 1);

    let empty = GraphBuilder::new().build().unwrap();
    let analysis = analyze(&empty);

    assert_eq!(analysis.critical_path_len(), Millis::ZERO);
    assert!(analysis.critical_path().is_empty());
    // An index past the end reads as absent rather than as unit zero. An identifier from another
    // graph that happens to be in range is not detected; see the graph tests.
    assert_eq!(analysis.earliest_start(foreign), None);
    assert_eq!(analysis.earliest_finish(foreign), None);
    assert_eq!(analysis.latest_finish(foreign), None);
    assert_eq!(analysis.slack(foreign), None);
    assert_eq!(analysis.critical_contribution(foreign), None);
    assert!(!analysis.is_critical(foreign));
}

#[test]
fn i11_single_unit_critical_path_is_its_own_weight() {
    let (graph, ids) = build(&[(500, Some(300), false)], &[]);
    let analysis = analyze(&graph);

    // One unit, nothing to wait for: it starts at 0 and the path is 500 long.
    assert_eq!(analysis.critical_path_len(), Millis::new(500));
    assert_eq!(analysis.critical_path(), &[ids[0]]);
    assert_eq!(analysis.earliest_start(ids[0]), Some(Millis::ZERO));
    assert_eq!(analysis.earliest_finish(ids[0]), Some(Millis::new(500)));
    assert_eq!(analysis.latest_finish(ids[0]), Some(Millis::new(500)));
    assert_eq!(analysis.slack(ids[0]), Some(Millis::ZERO));
    assert!(analysis.is_critical(ids[0]));
    // Nothing consumes it, so it contributes all of itself.
    assert_eq!(
        analysis.critical_contribution(ids[0]),
        Some(Millis::new(500))
    );
}

#[test]
fn i15_chain_of_full_edges_is_n_times_the_weight() {
    // Five units of 70 ms, each waiting for the previous one to finish completely.
    // CP = 5 * 70 = 350. Unit k starts at 70 * k.
    let specs: Vec<Spec> = (0..5).map(|_| (70, None, false)).collect();
    let edges: Vec<EdgeSpec> = (0..4).map(|i| (i, i + 1, EdgeKind::Full)).collect();
    let (graph, ids) = build(&specs, &edges);
    let analysis = analyze(&graph);

    assert_eq!(analysis.critical_path_len(), Millis::new(350));
    assert_eq!(analysis.critical_path(), ids.as_slice());
    for (k, id) in ids.iter().enumerate() {
        assert_eq!(
            analysis.earliest_start(*id),
            Some(Millis::new(70 * k as u64)),
            "unit {k}"
        );
        assert_eq!(analysis.slack(*id), Some(Millis::ZERO), "unit {k}");
        assert_eq!(
            analysis.critical_contribution(*id),
            Some(Millis::new(70)),
            "unit {k}"
        );
    }
}

#[test]
fn i15_fan_out_is_the_source_plus_the_largest_sink() {
    // One source of 100 ms feeding three sinks of 40, 90 and 25 ms over full edges.
    // CP = 100 + max(40, 90, 25) = 100 + 90 = 190.
    let (graph, ids) = build(
        &[
            (100, None, false),
            (40, None, false),
            (90, None, false),
            (25, None, false),
        ],
        &[
            (0, 1, EdgeKind::Full),
            (0, 2, EdgeKind::Full),
            (0, 3, EdgeKind::Full),
        ],
    );
    let analysis = analyze(&graph);

    assert_eq!(analysis.critical_path_len(), Millis::new(190));
    assert_eq!(analysis.critical_path(), &[ids[0], ids[2]]);
    // The 40 ms sink finishes at 140 and may finish as late as 190: slack 190 - 140 = 50.
    assert_eq!(analysis.slack(ids[1]), Some(Millis::new(50)));
    // The 25 ms sink finishes at 125: slack 190 - 125 = 65.
    assert_eq!(analysis.slack(ids[3]), Some(Millis::new(65)));
    assert_eq!(analysis.slack(ids[2]), Some(Millis::ZERO));
    assert!(analysis.is_critical(ids[0]));
    assert!(!analysis.is_critical(ids[1]));
}

#[test]
fn metadata_edges_credit_pipelining_where_full_edges_would_not() {
    // a (total 100, metadata at 30) -> b (total 60, metadata at 20) -> c (total 80).
    //
    // Over metadata edges:
    //   est(a) = 0,               eft(a) = 100
    //   est(b) = 0 + 30 = 30,     eft(b) = 90
    //   est(c) = 30 + 20 = 50,    eft(c) = 130
    //   CP = max(100, 90, 130) = 130
    let metadata_chain: Vec<EdgeSpec> =
        vec![(0, 1, EdgeKind::Metadata), (1, 2, EdgeKind::Metadata)];
    let specs: Vec<Spec> = vec![
        (100, Some(30), false),
        (60, Some(20), false),
        (80, Some(80), false),
    ];
    let (graph, ids) = build(&specs, &metadata_chain);
    let analysis = analyze(&graph);

    assert_eq!(analysis.critical_path_len(), Millis::new(130));
    assert_eq!(analysis.earliest_start(ids[1]), Some(Millis::new(30)));
    assert_eq!(analysis.earliest_start(ids[2]), Some(Millis::new(50)));
    assert_eq!(analysis.critical_path(), ids.as_slice());
    // Traversing a unit over a metadata edge costs its metadata offset, not its total:
    // 30 + 20 + 80 = 130, which is the path length.
    assert_eq!(
        analysis.critical_contribution(ids[0]),
        Some(Millis::new(30))
    );
    assert_eq!(
        analysis.critical_contribution(ids[1]),
        Some(Millis::new(20))
    );
    assert_eq!(
        analysis.critical_contribution(ids[2]),
        Some(Millis::new(80))
    );
    let sum: Millis = ids
        .iter()
        .filter_map(|id| analysis.critical_contribution(*id))
        .sum();
    assert_eq!(sum, analysis.critical_path_len());

    // The same three units joined by full edges instead: 100 + 60 + 80 = 240. The whole point of
    // the metadata semantics is the 110 ms difference (I13).
    let full_chain: Vec<EdgeSpec> = vec![(0, 1, EdgeKind::Full), (1, 2, EdgeKind::Full)];
    let (full_graph, _) = build(&specs, &full_chain);
    let full = analyze(&full_graph);
    assert_eq!(full.critical_path_len(), Millis::new(240));
    assert!(analysis.critical_path_len() < full.critical_path_len());
}

#[test]
fn i13_reclassifying_a_full_edge_as_metadata_cannot_lengthen_the_path() {
    // Diamond: a (100, metadata 40) over b (50, metadata 20) and c (80, metadata 30), both
    // feeding d (60) over full edges.
    //
    // All four edges full:      est(d) = max(100 + 50, 100 + 80) = 180, CP = 180 + 60 = 240
    // Front two edges metadata: est(b) = est(c) = 40, eft(b) = 90, eft(c) = 120,
    //                           est(d) = max(90, 120) = 120, CP = 180
    let specs: Vec<Spec> = vec![
        (100, Some(40), false),
        (50, Some(20), false),
        (80, Some(30), false),
        (60, None, false),
    ];
    let (all_full, _) = build(
        &specs,
        &[
            (0, 1, EdgeKind::Full),
            (0, 2, EdgeKind::Full),
            (1, 3, EdgeKind::Full),
            (2, 3, EdgeKind::Full),
        ],
    );
    let (front_metadata, ids) = build(
        &specs,
        &[
            (0, 1, EdgeKind::Metadata),
            (0, 2, EdgeKind::Metadata),
            (1, 3, EdgeKind::Full),
            (2, 3, EdgeKind::Full),
        ],
    );

    let strict = analyze(&all_full);
    let pipelined = analyze(&front_metadata);
    assert_eq!(strict.critical_path_len(), Millis::new(240));
    assert_eq!(pipelined.critical_path_len(), Millis::new(180));
    assert!(pipelined.critical_path_len() <= strict.critical_path_len());

    // In the pipelined diamond the path runs a -> c -> d, and b has slack.
    assert_eq!(pipelined.critical_path(), &[ids[0], ids[2], ids[3]]);
    // b finishes at 90 and could finish as late as 120, when d must start: slack 30.
    assert_eq!(pipelined.slack(ids[1]), Some(Millis::new(30)));
    assert_eq!(pipelined.latest_finish(ids[1]), Some(Millis::new(120)));
    // a must emit metadata by est(c) = 40, and it emits at 40, so it cannot be delayed at all
    // even though its own finish at 100 is well before d starts.
    assert_eq!(pipelined.latest_finish(ids[0]), Some(Millis::new(100)));
    assert_eq!(pipelined.slack(ids[0]), Some(Millis::ZERO));
    assert_eq!(pipelined.slack(ids[2]), Some(Millis::ZERO));
    assert_eq!(pipelined.slack(ids[3]), Some(Millis::ZERO));
}

#[test]
fn i14_increasing_a_weight_cannot_shorten_the_path() {
    // The pipelined diamond above, CP = 180. Raise b from 50 to 200:
    //   est(b) = 40, eft(b) = 240, est(d) = max(240, 120) = 240, CP = 240 + 60 = 300.
    let edges: Vec<EdgeSpec> = vec![
        (0, 1, EdgeKind::Metadata),
        (0, 2, EdgeKind::Metadata),
        (1, 3, EdgeKind::Full),
        (2, 3, EdgeKind::Full),
    ];
    let (before, _) = build(
        &[
            (100, Some(40), false),
            (50, Some(20), false),
            (80, Some(30), false),
            (60, None, false),
        ],
        &edges,
    );
    let (after, _) = build(
        &[
            (100, Some(40), false),
            (200, Some(20), false),
            (80, Some(30), false),
            (60, None, false),
        ],
        &edges,
    );

    assert_eq!(analyze(&before).critical_path_len(), Millis::new(180));
    assert_eq!(analyze(&after).critical_path_len(), Millis::new(300));
}

/// The measured fixture of `docs/model.md` 3.1, in milliseconds.
fn fixture() -> (BuildGraph, Vec<UnitId>) {
    build(
        &[
            (1240, Some(780), false), // 0: biga, a large library
            (60, Some(20), false),    // 1: mid, a trivial library over it
            (210, None, true),        // 2: pmc, a proc-macro over mid
            (30, Some(30), false),    // 3: toplib, an ordinary library over mid
        ],
        &[
            (0, 1, EdgeKind::Metadata),
            (1, 2, EdgeKind::Full),
            (1, 3, EdgeKind::Metadata),
        ],
    )
}

#[test]
fn closure_rule_reproduces_the_measured_fixture() {
    // These are the numbers a real Cargo build produced, rounded to the 10 ms the input carries.
    //
    //   est(biga)   = 0                              eft = 1240
    //   est(mid)    = 0 + metadata_at(biga) = 780    eft = 840
    //   est(toplib) = est(mid) + metadata_at(mid) = 780 + 20 = 800
    //   est(pmc)    = the whole closure of pmc is {mid, biga}, so
    //                 max(eft(mid), eft(biga)) = max(840, 1240) = 1240
    //   CP = max(1240, 840, 1450, 830) = 1450, attained by pmc.
    let (graph, ids) = fixture();
    let (biga, mid, pmc, toplib) = (ids[0], ids[1], ids[2], ids[3]);
    let analysis = analyze(&graph);

    assert_eq!(analysis.earliest_start(toplib), Some(Millis::new(800)));
    assert_eq!(analysis.earliest_start(pmc), Some(Millis::new(1240)));
    assert_eq!(analysis.earliest_start(mid), Some(Millis::new(780)));
    assert_eq!(analysis.earliest_finish(pmc), Some(Millis::new(1450)));
    assert_eq!(analysis.critical_path_len(), Millis::new(1450));

    // The path is biga then pmc: pmc waits for biga's full end, and mid contributes nothing to
    // that constraint even though it sits between them.
    assert_eq!(analysis.critical_path(), &[biga, pmc]);
    assert_eq!(
        analysis.critical_contribution(biga),
        Some(Millis::new(1240))
    );
    assert_eq!(analysis.critical_contribution(pmc), Some(Millis::new(210)));
    assert_eq!(analysis.critical_contribution(mid), None);
    assert_eq!(analysis.critical_contribution(toplib), None);

    // mid finishes at 840 but need not finish before pmc starts at 1240: slack 400.
    assert_eq!(analysis.slack(mid), Some(Millis::new(400)));
    assert_eq!(analysis.latest_finish(mid), Some(Millis::new(1240)));
    // toplib finishes at 830 and nothing waits for it: slack 1450 - 830 = 620.
    assert_eq!(analysis.slack(toplib), Some(Millis::new(620)));
    assert_eq!(analysis.slack(biga), Some(Millis::ZERO));
    assert_eq!(analysis.slack(pmc), Some(Millis::ZERO));
    assert!(analysis.is_critical(biga));
    assert!(!analysis.is_critical(mid));
}

#[test]
fn i17_a_closure_requirement_reaches_past_the_direct_dependencies() {
    let (graph, ids) = fixture();
    let (biga, pmc) = (ids[0], ids[2]);
    let analysis = analyze(&graph);

    // biga is not named as a dependency of pmc.
    assert!(graph
        .dependencies(pmc)
        .unwrap()
        .iter()
        .all(|dep| dep.unit != biga));
    // It still holds pmc back, because it is in pmc's closure: est(pmc) >= eft(biga) = 1240.
    assert!(analysis.earliest_start(pmc).unwrap() >= analysis.earliest_finish(biga).unwrap());

    // Every unit in the closure, not merely the direct dependencies.
    for dep in enumerated_closure(&graph, pmc) {
        assert!(
            analysis.earliest_start(pmc).unwrap() >= analysis.earliest_finish(dep).unwrap(),
            "unit {dep} in the closure of pmc finishes after pmc starts"
        );
    }
}

#[test]
fn i16_setting_a_closure_requirement_can_only_delay() {
    // The fixture with pmc's closure requirement cleared: pmc then waits only for its direct full
    // edge from mid, so est(pmc) = est(mid) + total(mid) = 780 + 60 = 840, eft = 1050, and
    // CP = max(1240, 840, 1050, 830) = 1240, which is 210 shorter than the 1450 with the
    // requirement in place.
    let (with_closure, _) = fixture();
    let (without_closure, ids) = build(
        &[
            (1240, Some(780), false),
            (60, Some(20), false),
            (210, None, false),
            (30, Some(30), false),
        ],
        &[
            (0, 1, EdgeKind::Metadata),
            (1, 2, EdgeKind::Full),
            (1, 3, EdgeKind::Metadata),
        ],
    );

    let strict = analyze(&with_closure);
    let relaxed = analyze(&without_closure);
    assert_eq!(relaxed.earliest_start(ids[2]), Some(Millis::new(840)));
    assert_eq!(relaxed.critical_path_len(), Millis::new(1240));
    assert_eq!(strict.critical_path_len(), Millis::new(1450));
    assert!(strict.critical_path_len() >= relaxed.critical_path_len());
}

#[test]
fn a_closure_requirement_overrides_the_kind_of_an_incoming_edge() {
    // u0 (100, metadata at 50) -> u1 (80, metadata at 40, needs its closure) -> u2 (60), both
    // edges metadata. If the edge kind decided, u1 would start at 50. The closure rule ignores the
    // edge kind and makes u1 wait for u0 to finish:
    //   est(u1) = eft(u0) = 100, eft(u1) = 180
    //   est(u2) = est(u1) + metadata_at(u1) = 100 + 40 = 140, eft(u2) = 200
    // u1 emitting metadata still helps u2, which has no closure requirement of its own.
    let (graph, ids) = build(
        &[
            (100, Some(50), false),
            (80, Some(40), true),
            (60, Some(10), false),
        ],
        &[(0, 1, EdgeKind::Metadata), (1, 2, EdgeKind::Metadata)],
    );
    let analysis = analyze(&graph);

    assert_eq!(analysis.earliest_start(ids[1]), Some(Millis::new(100)));
    assert_eq!(analysis.earliest_start(ids[2]), Some(Millis::new(140)));
    assert_eq!(analysis.critical_path_len(), Millis::new(200));
    assert_eq!(analysis.critical_path(), ids.as_slice());
    // u0 is traversed at its full length because the closure requirement made it a full
    // constraint, while u1 is traversed at its metadata offset: 100 + 40 + 60 = 200.
    assert_eq!(
        analysis.critical_contribution(ids[0]),
        Some(Millis::new(100))
    );
    assert_eq!(
        analysis.critical_contribution(ids[1]),
        Some(Millis::new(40))
    );
    assert_eq!(
        analysis.critical_contribution(ids[2]),
        Some(Millis::new(60))
    );
}

#[test]
fn i18_the_closure_recurrence_equals_the_enumerated_closure() {
    // For a unit that requires its full closure, est is exactly the latest finish anywhere in that
    // closure. Marking one unit at a time and comparing against an explicit enumeration is a
    // differential test of the recurrence that never builds the closure.
    for (name, specs, edges) in shapes() {
        let (base_graph, _) = build(&specs, &edges);
        let base = analyze(&base_graph);

        for target in 0..specs.len() {
            let mut marked = specs.clone();
            marked[target].2 = true;
            let (graph, ids) = build(&marked, &edges);
            let analysis = analyze(&graph);

            // Marking `target` delays only target and what follows it, and every unit in target's
            // closure lies strictly before it, so their finish times are the ones the base
            // analysis reported.
            let expected = enumerated_closure(&graph, ids[target])
                .iter()
                .map(|dep| base.earliest_finish(*dep).unwrap())
                .max()
                .unwrap_or(Millis::ZERO);

            assert_eq!(
                analysis.earliest_start(ids[target]),
                Some(expected),
                "{name}: unit {target} marked as requiring its full closure"
            );
        }
    }
}

#[test]
fn earliest_times_match_a_naive_reference_on_every_shape() {
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        let analysis = analyze(&graph);
        let (est, eft) = naive_times(&graph);

        for (i, id) in ids.iter().enumerate() {
            assert_eq!(
                analysis.earliest_start(*id),
                Some(Millis::new(est[i])),
                "{name}: earliest start of unit {i}"
            );
            assert_eq!(
                analysis.earliest_finish(*id),
                Some(Millis::new(eft[i])),
                "{name}: earliest finish of unit {i}"
            );
        }
        let longest = eft.iter().copied().max().unwrap_or(0);
        assert_eq!(
            analysis.critical_path_len(),
            Millis::new(longest),
            "{name}: critical path length"
        );
    }
}

#[test]
fn i9_slack_is_zero_on_a_critical_path_and_positive_beside_it() {
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        let analysis = analyze(&graph);
        let cp = analysis.critical_path_len();

        for id in &ids {
            let eft = analysis.earliest_finish(*id).unwrap();
            let lft = analysis.latest_finish(*id).unwrap();
            let slack = analysis.slack(*id).unwrap();
            // Slack is the difference of two reported times, and nothing may finish after the
            // critical path length without lengthening it.
            assert!(
                lft >= eft,
                "{name}: unit {id} has latest finish before earliest"
            );
            assert_eq!(
                slack,
                Millis::new(lft.get() - eft.get()),
                "{name}: unit {id}"
            );
            assert!(
                lft <= cp,
                "{name}: unit {id} may finish after the critical path"
            );
            assert_eq!(
                analysis.is_critical(*id),
                slack == Millis::ZERO,
                "{name}: unit {id}"
            );
        }
        // Every unit on the recovered path has no slack, and the contributions of the path sum to
        // its length.
        let mut sum = Millis::ZERO;
        for id in analysis.critical_path() {
            assert_eq!(analysis.slack(*id), Some(Millis::ZERO), "{name}: unit {id}");
            sum = sum.saturating_add(analysis.critical_contribution(*id).unwrap());
        }
        assert_eq!(sum, cp, "{name}: contributions of the path");
    }
}

#[test]
fn slack_can_actually_be_taken() {
    // Slack claims a unit may finish that much later without lengthening the build. Spend it:
    // lengthen the unit by exactly its slack and the critical path must not move.
    //
    // The proof this checks: after the change every unit still starts no later than its latest
    // start, because lft(U) <= lst(C) for a full edge and lst(U) + metadata_at(U) <= lst(C) for a
    // metadata edge, so the lengthened finish still clears every downstream constraint.
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        let analysis = analyze(&graph);
        let cp = analysis.critical_path_len();

        for (i, id) in ids.iter().enumerate() {
            let slack = analysis.slack(*id).unwrap().get();
            let mut widened = specs.clone();
            widened[i].0 += slack;
            // The metadata offset stays where it was: a unit that takes longer does not emit its
            // metadata later, which is exactly why the slack of a pipelined producer is not the
            // amount by which it may be lengthened.
            let (wider, _) = build(&widened, &edges);
            assert_eq!(
                analyze(&wider).critical_path_len(),
                cp,
                "{name}: unit {i} lengthened by its slack of {slack}"
            );
        }
    }
}

#[test]
fn slack_is_tight_against_a_delayed_start() {
    // The other half: slack must not be overstated either. Force the unit to start `slack + 1`
    // late by hanging a fresh source in front of it over a full edge, and the critical path must
    // grow by exactly one millisecond.
    //
    // Starting a unit at lst(U) + 1 puts the end of the longest path through it at
    // lst(U) + 1 + (CP - lst(U)) = CP + 1, and the added source finishes at lst(U) + 1, which is
    // at most CP - total(U) + 1, so nothing else can overtake it.
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        let analysis = analyze(&graph);
        let cp = analysis.critical_path_len();

        for (i, id) in ids.iter().enumerate() {
            let delayed_start = analysis.earliest_start(*id).unwrap().get()
                + analysis.slack(*id).unwrap().get()
                + 1;
            let mut with_brake = specs.clone();
            with_brake.push((delayed_start, None, false));
            let brake = with_brake.len() - 1;
            let mut braked_edges = edges.clone();
            braked_edges.push((brake, i, EdgeKind::Full));

            let (braked, _) = build(&with_brake, &braked_edges);
            let braked = analyze(&braked);
            assert_eq!(
                braked.critical_path_len(),
                Millis::new(cp.get() + 1),
                "{name}: unit {i} forced to start at {delayed_start}"
            );
        }
    }
}

#[test]
fn every_step_of_the_critical_path_is_a_constraint_the_model_states() {
    // The path is only meaningful if consecutive units are joined by something real: either an
    // edge whose release explains the successor's start, or a closure requirement that reaches
    // past the edges.
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        let analysis = analyze(&graph);
        let path = analysis.critical_path();
        assert!(!path.is_empty(), "{name}: no path");
        assert_eq!(
            analysis.earliest_start(path[0]),
            Some(Millis::ZERO),
            "{name}: the path does not start at time zero"
        );

        for pair in path.windows(2) {
            let (producer, consumer) = (pair[0], pair[1]);
            let start = analysis.earliest_start(consumer).unwrap();
            let contribution = analysis.critical_contribution(producer).unwrap();
            assert_eq!(
                analysis
                    .earliest_start(producer)
                    .unwrap()
                    .saturating_add(contribution),
                start,
                "{name}: {producer} does not explain the start of {consumer}"
            );

            if graph.unit(consumer).unwrap().requires_full_closure {
                assert!(
                    enumerated_closure(&graph, consumer).contains(&producer),
                    "{name}: {producer} is not in the closure of {consumer}"
                );
            } else {
                let dep = graph
                    .dependencies(consumer)
                    .unwrap()
                    .iter()
                    .find(|d| d.unit == producer)
                    .unwrap_or_else(|| panic!("{name}: no edge from {producer} to {consumer}"));
                let unit = graph.unit(producer).unwrap();
                let offset = match dep.kind {
                    EdgeKind::Metadata => unit.metadata_at.unwrap(),
                    EdgeKind::Full => unit.total,
                };
                assert_eq!(contribution, offset, "{name}: contribution of {producer}");
            }
        }
        // The final unit contributes all of itself, and the path ends where the build does.
        let last = *path.last().unwrap();
        assert_eq!(
            analysis.critical_contribution(last),
            Some(graph.unit(last).unwrap().total),
            "{name}: the last unit on the path"
        );
        let latest_finish = ids
            .iter()
            .map(|id| analysis.earliest_finish(*id).unwrap())
            .max()
            .unwrap();
        assert_eq!(
            analysis.earliest_finish(last),
            Some(latest_finish),
            "{name}: the path does not end at the last unit to finish"
        );
    }
}

#[test]
fn i1_repeated_analysis_of_the_same_graph_agrees_exactly() {
    for (name, specs, edges) in shapes() {
        let (graph, ids) = build(&specs, &edges);
        let first = analyze(&graph);
        let second = analyze(&graph);

        assert_eq!(
            first.critical_path_len(),
            second.critical_path_len(),
            "{name}"
        );
        assert_eq!(first.critical_path(), second.critical_path(), "{name}");
        for id in &ids {
            assert_eq!(
                first.earliest_start(*id),
                second.earliest_start(*id),
                "{name}"
            );
            assert_eq!(first.slack(*id), second.slack(*id), "{name}");
            assert_eq!(
                first.critical_contribution(*id),
                second.critical_contribution(*id),
                "{name}"
            );
        }
    }
}

#[test]
fn ties_for_the_longest_path_resolve_to_the_lowest_identifier() {
    // Two independent chains of the same length: a (30) -> b (40) and c (30) -> d (40), all full
    // edges. Both chains end at 70, so both b and d attain CP = 70. The reported path is the one
    // through the lower identifier, so that the output does not depend on iteration order.
    let (graph, ids) = build(
        &[
            (30, None, false),
            (40, None, false),
            (30, None, false),
            (40, None, false),
        ],
        &[(0, 1, EdgeKind::Full), (2, 3, EdgeKind::Full)],
    );
    let analysis = analyze(&graph);

    assert_eq!(analysis.critical_path_len(), Millis::new(70));
    assert_eq!(analysis.critical_path(), &[ids[0], ids[1]]);
    // Both chains are critical: neither can be delayed without lengthening the build.
    for id in &ids {
        assert_eq!(analysis.slack(*id), Some(Millis::ZERO));
    }
}

#[test]
fn a_unit_with_no_dependents_carries_slack_up_to_the_critical_path() {
    // Source s (100) feeds a long chain and one stub. s -> long (200) full, s -> stub (10) full.
    // CP = 100 + 200 = 300. The stub finishes at 110 and may finish as late as 300: slack 190.
    let (graph, ids) = build(
        &[(100, None, false), (200, None, false), (10, None, false)],
        &[(0, 1, EdgeKind::Full), (0, 2, EdgeKind::Full)],
    );
    let analysis = analyze(&graph);

    assert_eq!(analysis.critical_path_len(), Millis::new(300));
    assert_eq!(analysis.latest_finish(ids[2]), Some(Millis::new(300)));
    assert_eq!(analysis.slack(ids[2]), Some(Millis::new(190)));
}

#[test]
fn a_unit_is_never_told_to_finish_before_it_can() {
    // A unit can always finish at its earliest possible time without delaying anything, so its
    // latest finish is never earlier than its earliest finish. Two units whose weights sum past
    // u64::MAX are where this last held: the critical path saturates, and a backward pass that
    // subtracts from the saturated value hands the first unit a deadline before it can be met.
    let half = 1u64 << 63;
    let (graph, _) = build(
        &[(half, None, false), (half, None, false)],
        &[(0, 1, EdgeKind::Full)],
    );

    let analysis = analyze(&graph);
    for (id, _) in graph.units() {
        let earliest = analysis.earliest_finish(id).unwrap();
        let latest = analysis.latest_finish(id).unwrap();
        assert!(
            latest >= earliest,
            "unit {id} may finish at {earliest} at the earliest but is told to finish by {latest}"
        );
    }
}
