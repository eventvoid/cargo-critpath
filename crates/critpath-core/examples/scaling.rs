//! Measures how the analysis scales, so that any claim about it has a number behind it.
//!
//! Run with `cargo run --release --example scaling -p critpath-core`.
//!
//! The synthetic graphs are shaped like a real build rather than like a random DAG: most units are
//! libraries joined by metadata edges, and roughly one unit in seventeen is a proc-macro that emits
//! no metadata and requires its whole transitive closure, which is the case the closure recurrence
//! has to handle.

// An example exists to print its measurements, and reporting a setup failure by returning it from
// main is clearer here than threading a Result through the reporting itself.
#![allow(clippy::print_stdout)]

use std::error::Error;
use std::num::{NonZeroU32, NonZeroU64};
use std::time::Instant;

use critpath_core::{
    analyze, simulate, BuildGraph, EdgeKind, GraphBuilder, Millis, Policy, Unit, UnitKind,
};

/// One unit in seventeen is a proc-macro: it emits no metadata and needs its full closure.
fn is_opaque(i: usize) -> bool {
    i % 17 == 0
}

fn weight(i: usize) -> u64 {
    (i.wrapping_mul(37) % 900 + 100) as u64
}

fn build(units: usize, fan_in: usize) -> Result<BuildGraph, Box<dyn Error>> {
    let tenths = NonZeroU64::new(10).ok_or("ten is not zero")?;
    let mut builder = GraphBuilder::with_capacity(units);
    let mut ids = Vec::with_capacity(units);
    for i in 0..units {
        let total = weight(i);
        let unit = Unit {
            package: format!("p{i}"),
            version: "1.0.0".to_owned(),
            target: format!("t{i}"),
            kind: if is_opaque(i) {
                UnitKind::ProcMacro
            } else {
                UnitKind::Lib
            },
            total: Millis::new(total),
            // A library emits metadata about seven tenths of the way through, which is what the
            // dev-profile builds measured in docs/validation.md do.
            metadata_at: if is_opaque(i) {
                None
            } else {
                Some(Millis::new(total).scaled(7, tenths))
            },
            requires_full_closure: is_opaque(i),
        };
        ids.push(builder.add_unit(unit)?);
    }
    for i in 0..units {
        for k in 1..=fan_in {
            let Some(back) = i.checked_sub(k * 3) else {
                continue;
            };
            let (Some(&from), Some(&to)) = (ids.get(back), ids.get(i)) else {
                continue;
            };
            // A metadata edge requires the producer to emit metadata and the consumer not to link.
            let kind = if !is_opaque(back) && !is_opaque(i) {
                EdgeKind::Metadata
            } else {
                EdgeKind::Full
            };
            builder.add_edge(from, to, kind)?;
        }
    }
    Ok(builder.build()?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let executors = NonZeroU32::new(16).ok_or("16 is not zero")?;
    println!(
        "{:>7}  {:>8}  {:>10}  {:>10}  {:>10}",
        "units", "edges", "construct", "analyse", "simulate"
    );
    for &(units, fan_in) in &[(1_000, 4), (5_000, 4), (20_000, 6), (50_000, 6)] {
        let start = Instant::now();
        let graph = build(units, fan_in)?;
        let constructed = start.elapsed();

        let start = Instant::now();
        let analysis = analyze(&graph);
        let analysed = start.elapsed();

        let start = Instant::now();
        let schedule = simulate(&graph, executors, Policy::CargoLike);
        let simulated = start.elapsed();

        println!(
            "{:>7}  {:>8}  {:>10.1?}  {:>10.1?}  {:>10.1?}   critical path {}, makespan {}",
            units,
            graph.edge_count(),
            constructed,
            analysed,
            simulated,
            analysis.critical_path_len(),
            schedule.makespan(),
        );
    }
    Ok(())
}
