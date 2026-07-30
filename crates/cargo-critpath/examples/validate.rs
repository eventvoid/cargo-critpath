//! Regenerates the tables in `docs/validation.md` from captured builds.
//!
//! Usage: `cargo run --release --example validate -p cargo-critpath -- <capture-root>`
//!
//! A capture root holds one directory per build, named `<project>-j<jobs>-r<run>`, each containing
//! `verbose.log` and `cargo-timing.html`, plus one `<project>-metadata.json` per project. The
//! commands that produce them are in `docs/validation.md`. Captures are not committed: they run to
//! hundreds of megabytes and they are machine-specific.
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]
use cargo_critpath::parse_with_metadata;
use critpath_core::{analyze, area_bound, BuildGraph, EdgeKind, GraphBuilder, Unit};
use std::num::NonZeroU32;
use std::{env, fs};

fn rebuild(g: &BuildGraph, drop_closure: bool, all_full: bool) -> BuildGraph {
    let mut b = GraphBuilder::with_capacity(g.len());
    let mut ids = Vec::new();
    for (_, u) in g.units() {
        let mut u2: Unit = u.clone();
        if drop_closure {
            u2.requires_full_closure = false;
        }
        if all_full {
            u2.metadata_at = None;
        }
        ids.push(b.add_unit(u2).unwrap());
    }
    for (id, _) in g.units() {
        for d in g.dependencies(id).unwrap() {
            let kind = if all_full { EdgeKind::Full } else { d.kind };
            let (Some(&from), Some(&to)) = (ids.get(d.unit.index()), ids.get(id.index())) else {
                continue;
            };
            b.add_edge(from, to, kind).unwrap();
        }
    }
    b.build().unwrap()
}

fn main() {
    let root = env::args().nth(1).expect("capture root");
    for p in ["ripgrep", "async-graphql", "rust-analyzer", "bevy"] {
        let meta = fs::read_to_string(format!("{root}/{p}-metadata.json")).ok();
        for run in 1..=3 {
            let dir = format!("{root}/{p}-j10-r{run}");
            let (Ok(log), Ok(html)) = (
                fs::read_to_string(format!("{dir}/verbose.log")),
                fs::read_to_string(format!("{dir}/cargo-timing.html")),
            ) else {
                continue;
            };
            let input = parse_with_metadata(&log, &html, meta.as_deref()).unwrap();
            let g = &input.graph;
            let a = analyze(g);
            let j = NonZeroU32::new(10).unwrap();
            let no_closure = analyze(&rebuild(g, true, false)).critical_path_len().get();
            let no_pipe = analyze(&rebuild(g, false, true)).critical_path_len().get();
            let cp = a.critical_path_len().get();
            let path: Vec<String> = a
                .critical_path()
                .iter()
                .filter_map(|id| g.unit(*id).map(|u| format!("{}/{}", u.package, u.kind)))
                .collect();
            println!("{p:<15} r{run} work={:>7} CP={:>7} area10={:>7} span={:>7} par={:.2} noclos={:>7}(+{:.1}%) nopipe={:>7}(+{:.1}%) pathlen={} ",
                g.total_work().get(), cp, area_bound(g, j).get(),
                input.build.compilation_span.get(),
                g.total_work().get() as f64 / input.build.compilation_span.get() as f64,
                no_closure, 100.0*(cp as f64 - no_closure as f64)/cp as f64,
                no_pipe, 100.0*(no_pipe as f64 - cp as f64)/no_pipe as f64,
                path.len());
            println!("    path: {}", path.join(" -> "));
        }
    }
}
