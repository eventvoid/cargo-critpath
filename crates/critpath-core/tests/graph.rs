//! Construction and validation of [`BuildGraph`].
//!
//! Expected values are computed by hand in the test body. Nothing here calls the code under test
//! to decide what the answer should be.

// A test reports failure by panicking, and unwrapping a value the test has just asserted is
// present is clearer than propagating an error no caller can act on.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use critpath_core::{BuildGraph, EdgeKind, GraphBuilder, GraphError, Millis, Unit, UnitKind};

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

#[test]
fn empty_graph_is_valid_and_weighs_nothing() {
    let graph = GraphBuilder::new().build().unwrap();

    assert_eq!(graph.len(), 0);
    assert!(graph.is_empty());
    assert_eq!(graph.total_work(), Millis::ZERO);
    assert_eq!(graph.edge_count(), 0);
    assert_eq!(graph.topological_order(), &[]);
}

#[test]
fn single_unit_graph_reports_its_own_weight() {
    let mut b = GraphBuilder::new();
    let a = b.add_unit(lib("a", 500, 300)).unwrap();
    let graph = b.build().unwrap();

    assert_eq!(graph.len(), 1);
    assert!(!graph.is_empty());
    assert_eq!(graph.total_work(), Millis::new(500));
    assert_eq!(graph.dependencies(a).unwrap().len(), 0);
    assert_eq!(graph.dependents(a).unwrap().len(), 0);
    assert_eq!(graph.topological_order(), &[a]);
}

#[test]
fn total_work_is_the_sum_of_unit_weights() {
    let mut b = GraphBuilder::new();
    b.add_unit(lib("a", 100, 50)).unwrap();
    b.add_unit(lib("b", 250, 50)).unwrap();
    b.add_unit(opaque("c", 7, UnitKind::Bin)).unwrap();
    let graph = b.build().unwrap();

    // 100 + 250 + 7, computed here rather than by the code under test.
    assert_eq!(graph.total_work(), Millis::new(357));
}

#[test]
fn chain_records_dependencies_and_dependents_in_both_directions() {
    let mut b = GraphBuilder::new();
    let a = b.add_unit(lib("a", 100, 60)).unwrap();
    let c = b.add_unit(lib("c", 100, 60)).unwrap();
    let d = b.add_unit(lib("d", 100, 60)).unwrap();
    b.add_edge(a, c, EdgeKind::Metadata).unwrap();
    b.add_edge(c, d, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();

    assert_eq!(graph.edge_count(), 2);

    let into_c = graph.dependencies(c).unwrap();
    assert_eq!(into_c.len(), 1);
    assert_eq!(into_c[0].unit, a);
    assert_eq!(into_c[0].kind, EdgeKind::Metadata);

    let out_of_c = graph.dependents(c).unwrap();
    assert_eq!(out_of_c.len(), 1);
    assert_eq!(out_of_c[0].unit, d);
    assert_eq!(out_of_c[0].kind, EdgeKind::Full);

    assert_eq!(graph.dependencies(a).unwrap().len(), 0);
    assert_eq!(graph.dependents(d).unwrap().len(), 0);
    assert_eq!(graph.topological_order(), &[a, c, d]);
}

#[test]
fn topological_order_places_every_dependency_before_its_dependent() {
    // Insertion order is deliberately the reverse of a valid topological order.
    let mut b = GraphBuilder::new();
    let sink = b.add_unit(lib("sink", 10, 5)).unwrap();
    let mid = b.add_unit(lib("mid", 10, 5)).unwrap();
    let src = b.add_unit(lib("src", 10, 5)).unwrap();
    b.add_edge(src, mid, EdgeKind::Full).unwrap();
    b.add_edge(mid, sink, EdgeKind::Full).unwrap();
    let graph = b.build().unwrap();

    let order = graph.topological_order();
    let position = |u| order.iter().position(|&x| x == u).unwrap();
    assert!(position(src) < position(mid));
    assert!(position(mid) < position(sink));
}

#[test]
fn metadata_edge_from_a_unit_that_emits_no_metadata_is_rejected() {
    let mut b = GraphBuilder::new();
    let pm = b.add_unit(opaque("pm", 100, UnitKind::ProcMacro)).unwrap();
    let user = b.add_unit(lib("user", 100, 50)).unwrap();
    b.add_edge(pm, user, EdgeKind::Metadata).unwrap();

    match b.build() {
        Err(GraphError::MetadataEdgeFromOpaqueUnit { producer, consumer }) => {
            assert_eq!(producer, pm);
            assert_eq!(consumer, user);
        }
        other => panic!("expected MetadataEdgeFromOpaqueUnit, got {other:?}"),
    }
}

#[test]
fn metadata_emitted_after_the_unit_finishes_is_rejected() {
    let mut b = GraphBuilder::new();
    // 300ms of metadata inside a 200ms unit is not possible.
    let a = b.add_unit(lib("a", 200, 300)).unwrap();

    match b.build() {
        Err(GraphError::MetadataAfterCompletion {
            unit,
            metadata_at,
            total,
        }) => {
            assert_eq!(unit, a);
            assert_eq!(metadata_at, Millis::new(300));
            assert_eq!(total, Millis::new(200));
        }
        other => panic!("expected MetadataAfterCompletion, got {other:?}"),
    }
}

#[test]
fn a_unit_depending_on_itself_is_rejected() {
    let mut b = GraphBuilder::new();
    let a = b.add_unit(lib("a", 100, 50)).unwrap();

    match b.add_edge(a, a, EdgeKind::Full) {
        Err(GraphError::SelfDependency { unit }) => assert_eq!(unit, a),
        other => panic!("expected SelfDependency, got {other:?}"),
    }
}

#[test]
fn an_edge_naming_a_unit_that_does_not_exist_is_rejected() {
    let mut b = GraphBuilder::new();
    let a = b.add_unit(lib("a", 100, 50)).unwrap();
    let mut other_builder = GraphBuilder::new();
    other_builder.add_unit(lib("x", 1, 1)).unwrap();
    let stranger = other_builder.add_unit(lib("y", 1, 1)).unwrap();

    match b.add_edge(a, stranger, EdgeKind::Full) {
        Err(GraphError::UnknownUnit { id, units }) => {
            assert_eq!(id, stranger);
            assert_eq!(units, 1);
        }
        other => panic!("expected UnknownUnit, got {other:?}"),
    }
}

#[test]
fn a_cycle_is_rejected_and_the_cycle_is_reported() {
    let mut b = GraphBuilder::new();
    let a = b.add_unit(lib("a", 10, 5)).unwrap();
    let c = b.add_unit(lib("c", 10, 5)).unwrap();
    let d = b.add_unit(lib("d", 10, 5)).unwrap();
    b.add_edge(a, c, EdgeKind::Full).unwrap();
    b.add_edge(c, d, EdgeKind::Full).unwrap();
    b.add_edge(d, a, EdgeKind::Full).unwrap();

    match b.build() {
        Err(GraphError::Cycle { units }) => {
            // Every unit in this graph is on the only cycle there is.
            assert_eq!(units.len(), 3);
            for u in [a, c, d] {
                assert!(units.contains(&u), "cycle should mention {u:?}: {units:?}");
            }
        }
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn a_cycle_is_reported_even_when_part_of_the_graph_is_acyclic() {
    let mut b = GraphBuilder::new();
    let free = b.add_unit(lib("free", 10, 5)).unwrap();
    let x = b.add_unit(lib("x", 10, 5)).unwrap();
    let y = b.add_unit(lib("y", 10, 5)).unwrap();
    b.add_edge(free, x, EdgeKind::Full).unwrap();
    b.add_edge(x, y, EdgeKind::Full).unwrap();
    b.add_edge(y, x, EdgeKind::Full).unwrap();

    match b.build() {
        Err(GraphError::Cycle { units }) => {
            assert_eq!(units.len(), 2);
            assert!(units.contains(&x));
            assert!(units.contains(&y));
            assert!(!units.contains(&free), "free is not on the cycle: {units:?}");
        }
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn a_duplicate_edge_keeps_the_stronger_constraint_whichever_order_it_arrives() {
    // Full is stronger than Metadata: it releases the consumer later. A graph that states both
    // must behave as though only Full were stated, or the analysis would be optimistic.
    for (first, second) in [
        (EdgeKind::Metadata, EdgeKind::Full),
        (EdgeKind::Full, EdgeKind::Metadata),
    ] {
        let mut b = GraphBuilder::new();
        let a = b.add_unit(lib("a", 100, 50)).unwrap();
        let c = b.add_unit(lib("c", 100, 50)).unwrap();
        b.add_edge(a, c, first).unwrap();
        b.add_edge(a, c, second).unwrap();
        let graph = b.build().unwrap();

        assert_eq!(graph.edge_count(), 1, "{first:?} then {second:?}");
        let deps = graph.dependencies(c).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].kind, EdgeKind::Full, "{first:?} then {second:?}");
    }
}

#[test]
fn unit_lookup_outside_the_graph_returns_none_rather_than_panicking() {
    let mut b = GraphBuilder::new();
    b.add_unit(lib("a", 10, 5)).unwrap();
    let graph = b.build().unwrap();

    let mut other = GraphBuilder::new();
    other.add_unit(lib("x", 1, 1)).unwrap();
    other.add_unit(lib("y", 1, 1)).unwrap();
    let stranger = other.add_unit(lib("z", 1, 1)).unwrap();

    assert!(graph.unit(stranger).is_none());
    assert!(graph.dependencies(stranger).is_none());
    assert!(graph.dependents(stranger).is_none());
}

#[test]
fn units_iterates_every_unit_once_in_id_order() {
    let mut b = GraphBuilder::new();
    let a = b.add_unit(lib("a", 1, 1)).unwrap();
    let c = b.add_unit(lib("c", 2, 1)).unwrap();
    let d = b.add_unit(lib("d", 3, 1)).unwrap();
    let graph = b.build().unwrap();

    let seen: Vec<_> = graph.units().map(|(id, u)| (id, u.package.clone())).collect();
    assert_eq!(
        seen,
        vec![
            (a, "a".to_owned()),
            (c, "c".to_owned()),
            (d, "d".to_owned())
        ]
    );
}

#[test]
fn unit_ids_follow_insertion_order_so_adapters_control_the_canonical_order() {
    let mut b = GraphBuilder::new();
    let first = b.add_unit(lib("z", 1, 1)).unwrap();
    let second = b.add_unit(lib("a", 1, 1)).unwrap();

    assert_eq!(first.index(), 0);
    assert_eq!(second.index(), 1);

    let graph = b.build().unwrap();
    assert_eq!(graph.unit(first).unwrap().package, "z");
    assert_eq!(graph.unit(second).unwrap().package, "a");
}

#[test]
fn error_messages_name_the_units_involved() {
    let mut b = GraphBuilder::new();
    let pm = b.add_unit(opaque("pm", 100, UnitKind::ProcMacro)).unwrap();
    let user = b.add_unit(lib("user", 100, 50)).unwrap();
    b.add_edge(pm, user, EdgeKind::Metadata).unwrap();
    let message = b.build().unwrap_err().to_string();

    // The reader has to be able to tell which edge is wrong without a debugger.
    assert!(message.contains("metadata"), "{message}");
    for expected in ["0", "1"] {
        assert!(message.contains(expected), "{message}");
    }
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn graph_can_cross_thread_boundaries() {
    assert_send_sync::<BuildGraph>();
    assert_send_sync::<GraphError>();
}
