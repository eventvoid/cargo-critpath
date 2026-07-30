//! Counterfactual graph transformations.
//!
//! Every expected weight is computed by hand in the test body, with the arithmetic written out in
//! a comment. Nothing here calls the code under test to decide what the answer should be.

// A test reports failure by panicking, and unwrapping a value the test has just asserted is
// present is clearer than propagating an error no caller can act on.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use core::num::NonZeroU64;

use critpath_core::{
    apply, roots, Applied, BuildGraph, EdgeKind, GraphBuilder, Mapped, Millis, SplitDependents,
    Transform, TransformError, Unit, UnitId, UnitKind,
};

fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}

fn lib(name: &str, total_ms: u64, metadata_ms: u64) -> Unit {
    Unit {
        package: name.to_owned(),
        version: "1.0.0".to_owned(),
        target: name.to_owned(),
        kind: UnitKind::Lib,
        total: Millis::new(total_ms),
        metadata_at: Some(Millis::new(metadata_ms)),
        requires_full_closure: false,
    }
}

/// A unit that emits no metadata, so everything downstream of it waits for the whole thing.
fn opaque(name: &str, total_ms: u64, kind: UnitKind) -> Unit {
    Unit {
        package: name.to_owned(),
        version: "1.0.0".to_owned(),
        target: name.to_owned(),
        kind,
        total: Millis::new(total_ms),
        metadata_at: None,
        requires_full_closure: kind != UnitKind::Lib,
    }
}

/// The producers a unit depends on, in ascending order so the assertion does not depend on the
/// order the graph happens to store them in.
fn producers(graph: &BuildGraph, id: UnitId) -> Vec<UnitId> {
    let mut ids: Vec<UnitId> = graph
        .dependencies(id)
        .unwrap()
        .iter()
        .map(|dep| dep.unit)
        .collect();
    ids.sort();
    ids
}

fn targets(graph: &BuildGraph) -> Vec<String> {
    graph.units().map(|(_, u)| u.target.clone()).collect()
}

/// Everything about a graph that a transformation is allowed to decide, flattened so two graphs can
/// be compared for equality.
type Shape = (
    Vec<(String, UnitKind, u64, Option<u64>, bool)>,
    Vec<(usize, usize, EdgeKind)>,
);

fn shape(graph: &BuildGraph) -> Shape {
    let units = graph
        .units()
        .map(|(_, u)| {
            (
                u.target.clone(),
                u.kind,
                u.total.get(),
                u.metadata_at.map(Millis::get),
                u.requires_full_closure,
            )
        })
        .collect();
    let mut edges: Vec<(usize, usize, EdgeKind)> = Vec::new();
    for (consumer, _) in graph.units() {
        for dep in graph.dependencies(consumer).unwrap() {
            edges.push((dep.unit.index(), consumer.index(), dep.kind));
        }
    }
    edges.sort();
    (units, edges)
}

fn split_halves(applied: &Applied, old: UnitId) -> (UnitId, UnitId) {
    match applied.mapped(old) {
        Some(Mapped::Split { first, second }) => (first, second),
        other => panic!("expected unit {old} to have been split, found {other:?}"),
    }
}

fn kept(applied: &Applied, old: UnitId) -> UnitId {
    match applied.mapped(old) {
        Some(Mapped::Kept(new)) => new,
        other => panic!("expected unit {old} to have survived, found {other:?}"),
    }
}

/// An identifier from a graph of `count` units, which is out of range for any smaller graph. There
/// is no way to fabricate a [`UnitId`], so one has to come from somewhere.
fn last_unit_of_a_larger_graph(count: usize) -> UnitId {
    let mut builder = GraphBuilder::new();
    let mut ids = Vec::new();
    for i in 0..count {
        ids.push(builder.add_unit(lib(&format!("u{i}"), 10, 5)).unwrap());
    }
    let _ = builder.build().unwrap();
    *ids.last().unwrap()
}

#[test]
fn roots_are_the_units_nothing_depends_on() {
    let mut b = GraphBuilder::new();
    let script = b
        .add_unit(opaque("script", 100, UnitKind::BuildScriptCompile))
        .unwrap();
    let run = b
        .add_unit(opaque("run", 20, UnitKind::BuildScriptRun))
        .unwrap();
    let core = b.add_unit(lib("core", 400, 200)).unwrap();
    let app = b.add_unit(opaque("app", 50, UnitKind::Bin)).unwrap();
    let tool = b.add_unit(opaque("tool", 60, UnitKind::Bin)).unwrap();
    b.add_edge(script, run, EdgeKind::Full).unwrap();
    b.add_edge(run, core, EdgeKind::Full).unwrap();
    b.add_edge(core, app, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();

    // `app` and `tool` have no dependents; everything else feeds something.
    assert_eq!(roots(&graph), vec![app, tool]);
}

#[test]
fn roots_of_an_empty_graph_are_empty() {
    let graph = GraphBuilder::new().build().unwrap();

    assert_eq!(roots(&graph), Vec::new());
}

#[test]
fn pessimistic_split_halves_the_weight_and_feeds_every_dependent() {
    let mut b = GraphBuilder::new();
    let producer = b.add_unit(lib("producer", 100, 60)).unwrap();
    let target = b.add_unit(lib("target", 1000, 600)).unwrap();
    let first_dep = b.add_unit(lib("first_dep", 200, 100)).unwrap();
    let second_dep = b.add_unit(lib("second_dep", 300, 150)).unwrap();
    b.add_edge(producer, target, EdgeKind::Metadata).unwrap();
    b.add_edge(target, first_dep, EdgeKind::Metadata).unwrap();
    b.add_edge(target, second_dep, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();

    let applied = apply(
        &graph,
        &Transform::SplitUnit {
            unit: target,
            numerator: 1,
            denominator: nz(2),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::new(40),
        },
    )
    .unwrap();
    let split = applied.graph();
    let (first, second) = split_halves(&applied, target);

    // Work: 1000 * 1/2 = 500 for the first part, and the remainder 1000 - 500 = 500 for the
    // second. Each part then pays the 40ms overhead: 540 and 540.
    // Metadata: 600 * 1/2 = 300 for the first part and 600 * 1/2 = 300 for the second; both are
    // well below 540, so neither is clamped.
    let a = split.unit(first).unwrap();
    let z = split.unit(second).unwrap();
    assert_eq!(a.total, Millis::new(540));
    assert_eq!(z.total, Millis::new(540));
    assert_eq!(a.metadata_at, Some(Millis::new(300)));
    assert_eq!(z.metadata_at, Some(Millis::new(300)));
    assert_eq!(a.kind, UnitKind::Lib);
    assert_eq!(z.kind, UnitKind::Lib);
    assert!(!a.requires_full_closure);
    assert!(!z.requires_full_closure);
    assert_eq!(a.package, "target");
    assert_eq!(z.package, "target");

    // One unit became two.
    assert_eq!(split.len(), 5);

    // Edges: the producer feeds both parts (2), and under the pessimistic assumption each part
    // feeds each of the two dependents (4). Six in total, against three before.
    assert_eq!(graph.edge_count(), 3);
    assert_eq!(split.edge_count(), 6);

    let new_first_dep = kept(&applied, first_dep);
    let new_second_dep = kept(&applied, second_dep);
    let new_producer = kept(&applied, producer);
    assert_eq!(producers(split, new_first_dep), vec![first, second]);
    assert_eq!(producers(split, new_second_dep), vec![first, second]);
    assert_eq!(producers(split, first), vec![new_producer]);
    assert_eq!(producers(split, second), vec![new_producer]);

    // Outgoing edge kinds are inherited: the dependency of `first_dep` was a metadata edge, that
    // of `second_dep` a full edge.
    for dep in split.dependencies(new_first_dep).unwrap() {
        assert_eq!(dep.kind, EdgeKind::Metadata);
    }
    for dep in split.dependencies(new_second_dep).unwrap() {
        assert_eq!(dep.kind, EdgeKind::Full);
    }
    // The incoming metadata edge is inherited by both parts.
    for part in [first, second] {
        for dep in split.dependencies(part).unwrap() {
            assert_eq!(dep.kind, EdgeKind::Metadata);
        }
    }

    // 100 + 1000 + 200 + 300 = 1600 before; the two 540ms parts replace the 1000ms unit, so
    // 1600 - 1000 + 1080 = 1680 after, which is 1600 plus two overheads.
    assert_eq!(graph.total_work(), Millis::new(1600));
    assert_eq!(split.total_work(), Millis::new(1680));

    // The parts take the position the original unit held, so surrounding order is preserved.
    assert_eq!(
        targets(split),
        vec![
            "producer".to_owned(),
            "target (part a)".to_owned(),
            "target (part b)".to_owned(),
            "first_dep".to_owned(),
            "second_dep".to_owned(),
        ]
    );
}

#[test]
fn optimistic_split_gives_each_dependent_exactly_one_part() {
    let mut b = GraphBuilder::new();
    let target = b.add_unit(lib("target", 800, 400)).unwrap();
    let d0 = b.add_unit(lib("d0", 10, 5)).unwrap();
    let d1 = b.add_unit(lib("d1", 10, 5)).unwrap();
    let d2 = b.add_unit(lib("d2", 10, 5)).unwrap();
    let d3 = b.add_unit(lib("d3", 10, 5)).unwrap();
    for dependent in [d0, d1, d2, d3] {
        b.add_edge(target, dependent, EdgeKind::Metadata).unwrap();
    }
    let graph = b.build().unwrap();

    let applied = apply(
        &graph,
        &Transform::SplitUnit {
            unit: target,
            numerator: 1,
            denominator: nz(2),
            dependents: SplitDependents::Optimistic,
            overhead: Millis::ZERO,
        },
    )
    .unwrap();
    let split = applied.graph();
    let (first, second) = split_halves(&applied, target);

    // Every dependent attaches to the part that becomes available first. The two parts are equal
    // here, and a tie goes to part a.
    for dependent in [d0, d1, d2, d3] {
        assert_eq!(producers(split, kept(&applied, dependent)), vec![first]);
    }
    // Nothing at all depends on the part that was not chosen.
    assert!(split.dependents(second).unwrap().is_empty());

    // Four dependents, one incoming edge each, and no incoming edges anywhere else.
    assert_eq!(split.edge_count(), 4);
    for dependent in [d0, d1, d2, d3] {
        assert_eq!(
            split.dependencies(kept(&applied, dependent)).unwrap().len(),
            1
        );
    }

    // 800 * 1/2 = 400, remainder 800 - 400 = 400, no overhead.
    assert_eq!(split.unit(first).unwrap().total, Millis::new(400));
    assert_eq!(split.unit(second).unwrap().total, Millis::new(400));
}

#[test]
fn an_optimistic_split_attaches_every_dependent_to_the_part_that_finishes_first() {
    let mut b = GraphBuilder::new();
    let target = b.add_unit(lib("target", 800, 400)).unwrap();
    let d0 = b.add_unit(lib("d0", 10, 5)).unwrap();
    let d1 = b.add_unit(lib("d1", 10, 5)).unwrap();
    b.add_edge(target, d0, EdgeKind::Metadata).unwrap();
    b.add_edge(target, d1, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();

    let applied = apply(
        &graph,
        &Transform::SplitUnit {
            unit: target,
            numerator: 3,
            denominator: nz(4),
            dependents: SplitDependents::Optimistic,
            overhead: Millis::ZERO,
        },
    )
    .unwrap();
    let split = applied.graph();
    let (first, second) = split_halves(&applied, target);

    // Work: 800 * 3/4 = 600, remainder 800 - 600 = 200. Here it is the second part that is the
    // smaller one, so it is the second part that everything attaches to. Both parts inherit the
    // same incoming edges, so the smaller part is available first whatever an edge asks for, and
    // the same part is chosen for the metadata edge and the full edge alike.
    assert_eq!(split.unit(first).unwrap().total, Millis::new(600));
    assert_eq!(split.unit(second).unwrap().total, Millis::new(200));
    // Metadata: 400 * 3/4 = 300 and 400 * 1/4 = 100.
    assert_eq!(
        split.unit(first).unwrap().metadata_at,
        Some(Millis::new(300))
    );
    assert_eq!(
        split.unit(second).unwrap().metadata_at,
        Some(Millis::new(100))
    );

    assert_eq!(producers(split, kept(&applied, d0)), vec![second]);
    assert_eq!(producers(split, kept(&applied, d1)), vec![second]);
    assert!(split.dependents(first).unwrap().is_empty());
    assert_eq!(split.edge_count(), 2);
}

#[test]
fn split_by_one_third_rounds_to_nearest_and_costs_only_the_overhead() {
    let mut b = GraphBuilder::new();
    let target = b.add_unit(lib("target", 1000, 900)).unwrap();
    let graph = b.build().unwrap();

    let applied = apply(
        &graph,
        &Transform::SplitUnit {
            unit: target,
            numerator: 1,
            denominator: nz(3),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::new(25),
        },
    )
    .unwrap();
    let split = applied.graph();
    let (first, second) = split_halves(&applied, target);

    // Work: 1000 * 1/3 = 333.33..., rounded to nearest gives 333. The second part takes the
    // remainder, 1000 - 333 = 667, so the two parts still sum to exactly 1000.
    // With 25ms of overhead each: 333 + 25 = 358 and 667 + 25 = 692.
    assert_eq!(split.unit(first).unwrap().total, Millis::new(358));
    assert_eq!(split.unit(second).unwrap().total, Millis::new(692));

    // Metadata: 900 * 1/3 = 300 exactly, and 900 * 2/3 = 600 exactly. Neither exceeds its part.
    assert_eq!(
        split.unit(first).unwrap().metadata_at,
        Some(Millis::new(300))
    );
    assert_eq!(
        split.unit(second).unwrap().metadata_at,
        Some(Millis::new(600))
    );

    // Total work changed by exactly two overheads: 1000 + 2 * 25 = 1050.
    assert_eq!(graph.total_work(), Millis::new(1000));
    assert_eq!(split.total_work(), Millis::new(1050));
}

#[test]
fn split_clamps_metadata_to_the_part_that_carries_it() {
    let mut b = GraphBuilder::new();
    // A unit that emits its metadata only as it finishes: metadata_at == total.
    let target = b.add_unit(lib("target", 999, 999)).unwrap();
    let graph = b.build().unwrap();

    let applied = apply(
        &graph,
        &Transform::SplitUnit {
            unit: target,
            numerator: 1,
            denominator: nz(2),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::ZERO,
        },
    )
    .unwrap();
    let split = applied.graph();
    let (first, second) = split_halves(&applied, target);

    // Work: 999 * 1/2 = 499.5, rounded to nearest gives 500 for the first part; the second takes
    // the remainder 999 - 500 = 499.
    // Metadata: each part scales 999 by its own share of the denominator, and both shares are 1/2
    // here, so both give 999 * 1/2 = 500 (499.5 rounded). That fits the 500ms first part exactly,
    // but exceeds the 499ms second part, where it is clamped to 499 because a unit cannot emit
    // metadata after it has finished.
    assert_eq!(split.unit(first).unwrap().total, Millis::new(500));
    assert_eq!(
        split.unit(first).unwrap().metadata_at,
        Some(Millis::new(500))
    );
    assert_eq!(split.unit(second).unwrap().total, Millis::new(499));
    assert_eq!(
        split.unit(second).unwrap().metadata_at,
        Some(Millis::new(499))
    );
}

#[test]
fn splitting_a_unit_with_no_dependents_is_the_same_under_both_assumptions() {
    let mut b = GraphBuilder::new();
    let leaf = b.add_unit(lib("leaf", 100, 50)).unwrap();
    let root = b.add_unit(opaque("root", 400, UnitKind::Bin)).unwrap();
    b.add_edge(leaf, root, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();
    assert_eq!(roots(&graph), vec![root]);

    let split = |dependents| {
        apply(
            &graph,
            &Transform::SplitUnit {
                unit: root,
                numerator: 1,
                denominator: nz(2),
                dependents,
                overhead: Millis::new(10),
            },
        )
        .unwrap()
    };
    let optimistic = split(SplitDependents::Optimistic);
    let pessimistic = split(SplitDependents::Pessimistic);

    // Nothing depended on the split unit, so there is no dependent to assign and the two
    // assumptions cannot differ. An interval built from them alone would be a point.
    assert_eq!(shape(optimistic.graph()), shape(pessimistic.graph()));
    assert_eq!(
        optimistic.mapping().collect::<Vec<_>>(),
        pessimistic.mapping().collect::<Vec<_>>()
    );

    let split = optimistic.graph();
    let (first, second) = split_halves(&optimistic, root);
    // Neither part has a dependent, so both are roots.
    assert_eq!(roots(split), vec![first, second]);
    // 400 * 1/2 = 200, remainder 400 - 200 = 200, plus 10ms overhead each.
    assert_eq!(split.unit(first).unwrap().total, Millis::new(210));
    assert_eq!(split.unit(second).unwrap().total, Millis::new(210));
    // Both parts keep the incoming edge from the leaf, and the closure requirement.
    assert_eq!(split.edge_count(), 2);
    assert!(split.unit(first).unwrap().requires_full_closure);
    assert!(split.unit(second).unwrap().requires_full_closure);
    // A unit that emits no metadata cannot acquire any by being split.
    assert_eq!(split.unit(first).unwrap().metadata_at, None);
    assert_eq!(split.unit(second).unwrap().metadata_at, None);
}

#[test]
fn splitting_a_zero_weight_unit_yields_two_units_of_overhead() {
    let mut b = GraphBuilder::new();
    let target = b.add_unit(lib("target", 0, 0)).unwrap();
    let graph = b.build().unwrap();

    let applied = apply(
        &graph,
        &Transform::SplitUnit {
            unit: target,
            numerator: 1,
            denominator: nz(2),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::new(7),
        },
    )
    .unwrap();
    let split = applied.graph();
    let (first, second) = split_halves(&applied, target);

    // 0 * 1/2 = 0 and the remainder is 0, so each part is nothing but the 7ms overhead.
    assert_eq!(split.unit(first).unwrap().total, Millis::new(7));
    assert_eq!(split.unit(second).unwrap().total, Millis::new(7));
    assert_eq!(split.unit(first).unwrap().metadata_at, Some(Millis::ZERO));
    assert_eq!(split.total_work(), Millis::new(14));
}

#[test]
fn splitting_a_build_script_is_refused() {
    let mut b = GraphBuilder::new();
    let compile = b
        .add_unit(opaque("script", 300, UnitKind::BuildScriptCompile))
        .unwrap();
    let run = b
        .add_unit(opaque("run", 200, UnitKind::BuildScriptRun))
        .unwrap();
    b.add_edge(compile, run, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();

    for (unit, kind) in [
        (compile, UnitKind::BuildScriptCompile),
        (run, UnitKind::BuildScriptRun),
    ] {
        let error = apply(
            &graph,
            &Transform::SplitUnit {
                unit,
                numerator: 1,
                denominator: nz(2),
                dependents: SplitDependents::Pessimistic,
                overhead: Millis::ZERO,
            },
        )
        .unwrap_err();
        assert_eq!(error, TransformError::UnsplittableKind { unit, kind });
        assert!(error.to_string().contains("cannot be split"));
    }
}

#[test]
fn splitting_off_more_than_the_whole_unit_is_refused() {
    let mut b = GraphBuilder::new();
    let target = b.add_unit(lib("target", 500, 250)).unwrap();
    let graph = b.build().unwrap();

    let error = apply(
        &graph,
        &Transform::SplitUnit {
            unit: target,
            numerator: 4,
            denominator: nz(3),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::ZERO,
        },
    )
    .unwrap_err();

    assert_eq!(
        error,
        TransformError::FractionAboveOne {
            unit: target,
            numerator: 4,
            denominator: nz(3),
        }
    );
    assert!(error.to_string().contains("no larger than the denominator"));

    // The whole unit is allowed, degenerate though it is: one part carries everything.
    let applied = apply(
        &graph,
        &Transform::SplitUnit {
            unit: target,
            numerator: 3,
            denominator: nz(3),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::ZERO,
        },
    )
    .unwrap();
    let (first, second) = split_halves(&applied, target);
    // 500 * 3/3 = 500, remainder 500 - 500 = 0.
    assert_eq!(applied.graph().unit(first).unwrap().total, Millis::new(500));
    assert_eq!(applied.graph().unit(second).unwrap().total, Millis::ZERO);
}

#[test]
fn splitting_a_unit_from_another_graph_is_refused() {
    let mut small = GraphBuilder::new();
    let _ = small.add_unit(lib("only", 100, 50)).unwrap();
    let small = small.build().unwrap();

    let outside = last_unit_of_a_larger_graph(5);

    let error = apply(
        &small,
        &Transform::SplitUnit {
            unit: outside,
            numerator: 1,
            denominator: nz(2),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::ZERO,
        },
    )
    .unwrap_err();

    assert_eq!(
        error,
        TransformError::UnknownUnit {
            unit: outside,
            units: 1,
        }
    );
}

#[test]
fn removing_an_edge_deletes_that_edge_and_nothing_else() {
    let mut b = GraphBuilder::new();
    let a = b.add_unit(lib("a", 100, 50)).unwrap();
    let mid = b.add_unit(lib("mid", 200, 100)).unwrap();
    let end = b.add_unit(opaque("end", 300, UnitKind::Bin)).unwrap();
    b.add_edge(a, mid, EdgeKind::Metadata).unwrap();
    b.add_edge(mid, end, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();

    let applied = apply(&graph, &Transform::RemoveEdge { from: a, to: mid }).unwrap();
    let cut = applied.graph();

    assert_eq!(cut.len(), 3);
    assert_eq!(cut.edge_count(), 1);
    // 100 + 200 + 300 = 600; removing an edge moves no work.
    assert_eq!(cut.total_work(), Millis::new(600));
    assert_eq!(targets(cut), vec!["a", "mid", "end"]);
    // The surviving edge is mid -> end.
    assert_eq!(
        producers(cut, kept(&applied, end)),
        vec![kept(&applied, mid)]
    );
    assert!(cut.dependencies(kept(&applied, mid)).unwrap().is_empty());
    // `a` now feeds nothing, so it becomes a root: removing an edge does not prune units.
    assert_eq!(roots(cut), vec![kept(&applied, a), kept(&applied, end)]);
}

#[test]
fn removing_an_absent_edge_is_an_error() {
    let mut b = GraphBuilder::new();
    let a = b.add_unit(lib("a", 100, 50)).unwrap();
    let mid = b.add_unit(lib("mid", 200, 100)).unwrap();
    let end = b.add_unit(opaque("end", 300, UnitKind::Bin)).unwrap();
    b.add_edge(a, mid, EdgeKind::Metadata).unwrap();
    b.add_edge(mid, end, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();

    // `end` depends on `mid`, not on `a`, and the edge between `a` and `mid` runs the other way.
    for (from, to) in [(a, end), (mid, a)] {
        let error = apply(&graph, &Transform::RemoveEdge { from, to }).unwrap_err();
        assert_eq!(error, TransformError::MissingEdge { from, to });
        assert!(error.to_string().contains("no edge"));
    }
}

/// A graph where dropping `heavy` must also drop the two units that only existed to serve it,
/// while `shared` survives because the root depends on it directly as well.
fn dragged_in_graph() -> (BuildGraph, [UnitId; 5]) {
    let mut b = GraphBuilder::new();
    let deep = b.add_unit(lib("deep", 700, 300)).unwrap();
    let mid = b.add_unit(lib("mid", 500, 200)).unwrap();
    let heavy = b.add_unit(lib("heavy", 900, 400)).unwrap();
    let shared = b.add_unit(lib("shared", 150, 80)).unwrap();
    let app = b.add_unit(opaque("app", 250, UnitKind::Bin)).unwrap();
    b.add_edge(deep, mid, EdgeKind::Metadata).unwrap();
    b.add_edge(mid, heavy, EdgeKind::Metadata).unwrap();
    b.add_edge(shared, heavy, EdgeKind::Metadata).unwrap();
    b.add_edge(heavy, app, EdgeKind::Full).unwrap();
    b.add_edge(shared, app, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();
    (graph, [deep, mid, heavy, shared, app])
}

#[test]
fn removing_a_unit_also_removes_what_only_it_dragged_in() {
    let (graph, [deep, mid, heavy, shared, app]) = dragged_in_graph();
    assert_eq!(roots(&graph), vec![app]);

    let applied = apply(&graph, &Transform::RemoveUnits { units: vec![heavy] }).unwrap();
    let pruned = applied.graph();

    // The only root is `app`. Walking backwards from it reaches `shared` directly, and reaches
    // nothing else once `heavy` is gone. So the survivors are exactly `shared` and `app`; `deep`
    // and `mid` existed only for `heavy`.
    assert_eq!(targets(pruned), vec!["shared", "app"]);
    assert_eq!(pruned.len(), 2);
    // The one surviving edge is shared -> app.
    assert_eq!(pruned.edge_count(), 1);
    assert_eq!(
        producers(pruned, kept(&applied, app)),
        vec![kept(&applied, shared)]
    );
    // 150 + 250 = 400.
    assert_eq!(pruned.total_work(), Millis::new(400));

    assert_eq!(applied.mapped(deep), Some(Mapped::Removed));
    assert_eq!(applied.mapped(mid), Some(Mapped::Removed));
    assert_eq!(applied.mapped(heavy), Some(Mapped::Removed));
}

#[test]
fn the_mapping_relates_every_surviving_unit_to_its_original() {
    let (graph, [deep, mid, heavy, shared, app]) = dragged_in_graph();

    let applied = apply(&graph, &Transform::RemoveUnits { units: vec![heavy] }).unwrap();
    let pruned = applied.graph();

    // One entry per unit of the original graph, in the original order.
    assert_eq!(applied.mapping().len(), 5);
    assert_eq!(
        applied.mapping().map(|(old, _)| old).collect::<Vec<_>>(),
        vec![deep, mid, heavy, shared, app]
    );

    // Every surviving unit is named by the map, and the unit it names is the same unit.
    let mut survivors = 0;
    for (old, mapped) in applied.mapping() {
        if let Mapped::Kept(new) = mapped {
            survivors += 1;
            assert_eq!(
                graph.unit(old).unwrap().target,
                pruned.unit(new).unwrap().target
            );
        }
    }
    assert_eq!(survivors, 2);
    // `shared` came before `app` in the original, and that order is preserved.
    assert!(kept(&applied, shared).index() < kept(&applied, app).index());
}

#[test]
fn removing_no_units_changes_nothing() {
    let (graph, _) = dragged_in_graph();

    let applied = apply(&graph, &Transform::RemoveUnits { units: Vec::new() }).unwrap();

    // Every unit can reach the root, so nothing is pruned and the graph is unchanged.
    assert_eq!(shape(applied.graph()), shape(&graph));
}

#[test]
fn removing_every_unit_yields_an_empty_graph() {
    let (graph, units) = dragged_in_graph();

    let applied = apply(
        &graph,
        &Transform::RemoveUnits {
            units: units.to_vec(),
        },
    )
    .unwrap();
    let empty = applied.graph();

    assert_eq!(empty.len(), 0);
    assert!(empty.is_empty());
    assert_eq!(empty.edge_count(), 0);
    assert_eq!(empty.total_work(), Millis::ZERO);
    assert_eq!(roots(empty), Vec::new());
    for (_, mapped) in applied.mapping() {
        assert_eq!(mapped, Mapped::Removed);
    }
}

#[test]
fn removing_the_root_removes_everything_it_kept_alive() {
    let (graph, [_, _, _, _, app]) = dragged_in_graph();

    let applied = apply(&graph, &Transform::RemoveUnits { units: vec![app] }).unwrap();

    // `app` was the only root, so no surviving unit has anything left to be built for.
    assert_eq!(applied.graph().len(), 0);
}

#[test]
fn removing_the_same_unit_twice_is_not_an_error() {
    let (graph, [_, _, heavy, _, _]) = dragged_in_graph();

    let once = apply(&graph, &Transform::RemoveUnits { units: vec![heavy] }).unwrap();
    let twice = apply(
        &graph,
        &Transform::RemoveUnits {
            units: vec![heavy, heavy],
        },
    )
    .unwrap();

    assert_eq!(shape(once.graph()), shape(twice.graph()));
}

#[test]
fn removing_a_unit_from_another_graph_is_an_error() {
    let (graph, [_, _, heavy, _, _]) = dragged_in_graph();

    let outside = last_unit_of_a_larger_graph(9);

    let error = apply(
        &graph,
        &Transform::RemoveUnits {
            units: vec![heavy, outside],
        },
    )
    .unwrap_err();

    assert_eq!(
        error,
        TransformError::UnknownUnit {
            unit: outside,
            units: 5,
        }
    );
    assert!(error.to_string().contains("not part of this graph"));
}

#[test]
fn an_optimistic_split_leaves_the_edges_it_does_not_touch_alone() {
    let (graph, [deep, mid, heavy, shared, app]) = dragged_in_graph();

    let applied = apply(
        &graph,
        &Transform::SplitUnit {
            unit: heavy,
            numerator: 1,
            denominator: nz(4),
            dependents: SplitDependents::Optimistic,
            overhead: Millis::new(20),
        },
    )
    .unwrap();
    let split = applied.graph();
    let (first, second) = split_halves(&applied, heavy);

    // Work: 900 * 1/4 = 225 exactly, remainder 900 - 225 = 675, plus 20ms overhead each.
    assert_eq!(split.unit(first).unwrap().total, Millis::new(245));
    assert_eq!(split.unit(second).unwrap().total, Millis::new(695));
    // Metadata: 400 * 1/4 = 100 and 400 * 3/4 = 300, neither near its part's total.
    assert_eq!(
        split.unit(first).unwrap().metadata_at,
        Some(Millis::new(100))
    );
    assert_eq!(
        split.unit(second).unwrap().metadata_at,
        Some(Millis::new(300))
    );
    // 700 + 500 + 900 + 150 + 250 = 2500 before, and only the two overheads are added.
    assert_eq!(graph.total_work(), Millis::new(2500));
    assert_eq!(split.total_work(), Millis::new(2540));

    // `heavy` had two producers, and both parts inherit both of them: deep -> mid stays as it was,
    // mid and shared each feed both parts, `app` attaches to part a because at 1/4 part a is the
    // one that finishes first, and shared -> app is untouched. 1 + 2 + 2 + 1 + 1 = 7 edges.
    assert_eq!(split.edge_count(), 7);
    let new_mid = kept(&applied, mid);
    let new_shared = kept(&applied, shared);
    assert_eq!(producers(split, new_mid), vec![kept(&applied, deep)]);
    assert_eq!(producers(split, first), vec![new_mid, new_shared]);
    assert_eq!(producers(split, second), vec![new_mid, new_shared]);
    assert_eq!(
        producers(split, kept(&applied, app)),
        vec![first, new_shared]
    );
}

#[test]
fn the_mapping_can_be_read_without_matching_on_it() {
    let (graph, [deep, _, heavy, shared, _]) = dragged_in_graph();

    let split = apply(
        &graph,
        &Transform::SplitUnit {
            unit: heavy,
            numerator: 1,
            denominator: nz(2),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::ZERO,
        },
    )
    .unwrap();
    let (first, second) = split_halves(&split, heavy);
    // The first part stands for the original unit, and the second is reachable beside it.
    assert_eq!(split.mapped(heavy).unwrap().primary(), Some(first));
    assert_eq!(split.mapped(heavy).unwrap().secondary(), Some(second));
    assert!(!split.mapped(heavy).unwrap().is_removed());

    let pruned = apply(&graph, &Transform::RemoveUnits { units: vec![heavy] }).unwrap();
    // A unit that survived has one identifier and no second one.
    assert_eq!(
        pruned.mapped(shared).unwrap().primary(),
        Some(kept(&pruned, shared))
    );
    assert_eq!(pruned.mapped(shared).unwrap().secondary(), None);
    // A unit that was pruned has none.
    assert!(pruned.mapped(deep).unwrap().is_removed());
    assert_eq!(pruned.mapped(deep).unwrap().primary(), None);

    // The graph can be taken by value once the mapping has been read.
    assert_eq!(pruned.graph().len(), 2);
    assert_eq!(pruned.into_graph().len(), 2);
}

#[test]
fn the_same_transform_applied_twice_gives_identical_graphs() {
    let (graph, [_, _, heavy, _, app]) = dragged_in_graph();

    let transforms = [
        Transform::SplitUnit {
            unit: heavy,
            numerator: 2,
            denominator: nz(7),
            dependents: SplitDependents::Optimistic,
            overhead: Millis::new(33),
        },
        Transform::SplitUnit {
            unit: heavy,
            numerator: 2,
            denominator: nz(7),
            dependents: SplitDependents::Pessimistic,
            overhead: Millis::new(33),
        },
        Transform::RemoveUnits { units: vec![heavy] },
        Transform::RemoveEdge {
            from: heavy,
            to: app,
        },
    ];

    for transform in &transforms {
        let first = apply(&graph, transform).unwrap();
        let second = apply(&graph, transform).unwrap();
        assert_eq!(shape(first.graph()), shape(second.graph()));
        assert_eq!(
            first.mapping().collect::<Vec<_>>(),
            second.mapping().collect::<Vec<_>>()
        );
    }
}
