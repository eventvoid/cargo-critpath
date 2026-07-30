//! Two instances of one package in one build.
//!
//! Cargo builds a package more than once whenever the same name resolves to two versions, or when
//! one instance is needed for the host and another for the target. The timing report describes both
//! with the same name and the same display string, and a build-script execution has no source path
//! and no crate name of its own to tell them apart with. Getting this wrong silently swaps two
//! units' weights, which is the failure mode this whole adapter is organised against.
//!
//! Both cases below were found by running the adapter over a captured `rust-analyzer` build, which
//! contains twenty-eight such pairs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use critpath_core::{EdgeKind, Millis, UnitId};

/// Two versions of one package, each with a build script, listed in the report in an order that
/// does not agree with the order the log started them in.
fn two_versions() -> (String, String) {
    let log = "\
     Running `/tc/bin/rustc --crate-name build_script_build /reg/thiserror-1.0.69/build.rs \
     --crate-type bin --emit=dep-info,link -C extra-filename=-1111111111111111 \
     --out-dir /ws/target/debug/build/thiserror-1111111111111111`\n\
     Running `/tc/bin/rustc --crate-name build_script_build /reg/thiserror-2.0.18/build.rs \
     --crate-type bin --emit=dep-info,link -C extra-filename=-2222222222222222 \
     --out-dir /ws/target/debug/build/thiserror-2222222222222222`\n\
     Running `/ws/target/debug/build/thiserror-1111111111111111/build-script-build`\n\
     Running `/ws/target/debug/build/thiserror-2222222222222222/build-script-build`\n\
     Running `/tc/bin/rustc --crate-name thiserror /reg/thiserror-1.0.69/src/lib.rs \
     --crate-type lib --emit=dep-info,metadata,link -C extra-filename=-3333333333333333 \
     --out-dir /ws/target/debug/deps`\n\
     Running `/tc/bin/rustc --crate-name thiserror /reg/thiserror-2.0.18/src/lib.rs \
     --crate-type lib --emit=dep-info,metadata,link -C extra-filename=-4444444444444444 \
     --out-dir /ws/target/debug/deps`\n"
        .to_owned();

    let unit = |i: usize, version: &str, target: &str, start: f64, duration: f64| {
        format!(
            r#"{{"i":{i},"name":"thiserror","version":"{version}","mode":"todo",
                 "target":"{target}","features":[],"start":{start},"duration":{duration},
                 "unblocked_units":[],"unblocked_rmeta_units":[],"sections":null}}"#
        )
    };
    // The two executions appear in the opposite order to their compilations, which is what start
    // order alone would get wrong.
    let units = [
        unit(0, "1.0.69", " build-script", 0.10, 0.10),
        unit(1, "2.0.18", " build-script", 0.10, 0.20),
        unit(2, "2.0.18", " build-script (run)", 0.30, 0.70),
        unit(3, "1.0.69", " build-script (run)", 0.30, 0.30),
        unit(4, "1.0.69", "", 1.00, 0.40),
        unit(5, "2.0.18", "", 1.00, 0.50),
    ]
    .join(",\n");

    let report = format!(
        "<html><body><table>\n\
         <tr><td>Profile:</td><td>dev</td></tr>\n\
         <tr><td>Fresh units:</td><td>0</td></tr>\n\
         <tr><td>Dirty units:</td><td>6</td></tr>\n\
         <tr><td>Total units:</td><td>6</td></tr>\n\
         <tr><td>Max concurrency:</td><td>2 (jobs=2 ncpu=8)</td></tr>\n\
         <tr><td>Total time:</td><td>1.5s</td></tr>\n\
         </table>\n<script>const UNIT_DATA = [\n{units}\n];</script></body></html>\n"
    );
    (log, report)
}

fn unit_of(input: &cargo_critpath::BuildInput, version: &str, target: &str) -> UnitId {
    input
        .graph
        .units()
        .find(|(_, u)| u.version == version && u.target == target)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no unit for {version} ({target})"))
}

#[test]
fn a_build_script_execution_takes_its_identity_from_its_compilation() {
    let (log, report) = two_versions();
    let input = cargo_critpath::parse(&log, &report).unwrap();
    assert_eq!(input.graph.len(), 6);

    // The compilation and the execution of one version have to be the same version's, and the
    // execution's weight has to be the one the report recorded against that version: 0.30 s for
    // 1.0.69 and 0.70 s for 2.0.18, not the other way round.
    for (version, compile_ms, run_ms, lib_ms) in
        [("1.0.69", 100, 300, 400), ("2.0.18", 200, 700, 500)]
    {
        let compile = unit_of(&input, version, "build-script");
        let run = unit_of(&input, version, "build-script (run)");
        let lib = unit_of(&input, version, "lib");
        assert_eq!(
            input.graph.unit(compile).unwrap().total,
            Millis::new(compile_ms)
        );
        assert_eq!(input.graph.unit(run).unwrap().total, Millis::new(run_ms));
        assert_eq!(input.graph.unit(lib).unwrap().total, Millis::new(lib_ms));

        let has = |from: UnitId, to: UnitId| {
            input
                .graph
                .dependencies(to)
                .unwrap()
                .iter()
                .any(|d| d.unit == from && d.kind == EdgeKind::Full)
        };
        assert!(has(compile, run), "{version}: compile does not feed run");
        assert!(has(run, lib), "{version}: run does not feed lib");
    }

    // Six units and four edges: two build-script triples and nothing between the two versions.
    assert_eq!(input.graph.edge_count(), 4);
    // Nothing had to be guessed, so nothing is reported as guessed.
    assert!(input.diagnostics.is_empty(), "{:?}", input.diagnostics);
}

#[test]
fn units_that_cannot_be_told_apart_are_paired_by_order_and_said_to_be() {
    // The same package, the same version, the same features, twice. This is what a package built
    // once for the host and once for the target looks like, and nothing in either input separates
    // them.
    let log = "\
     Running `/tc/bin/rustc --crate-name twin /reg/twin-1.0.0/src/lib.rs --crate-type lib \
     --emit=dep-info,metadata,link -C extra-filename=-1111111111111111 \
     --out-dir /ws/target/debug/deps`\n\
     Running `/tc/bin/rustc --crate-name twin /reg/twin-1.0.0/src/lib.rs --crate-type lib \
     --emit=dep-info,metadata,link -C extra-filename=-2222222222222222 \
     --out-dir /ws/target/debug/deps`\n";
    let report = "<html><body><table>\n\
        <tr><td>Fresh units:</td><td>0</td></tr>\n\
        <tr><td>Dirty units:</td><td>2</td></tr>\n\
        <tr><td>Total units:</td><td>2</td></tr>\n\
        </table>\n<script>const UNIT_DATA = [\n\
        {\"i\":0,\"name\":\"twin\",\"version\":\"1.0.0\",\"target\":\"\",\"features\":[],\
        \"start\":0.0,\"duration\":0.5,\"unblocked_units\":[],\"unblocked_rmeta_units\":[],\
        \"sections\":null},\n\
        {\"i\":1,\"name\":\"twin\",\"version\":\"1.0.0\",\"target\":\"\",\"features\":[],\
        \"start\":0.1,\"duration\":0.9,\"unblocked_units\":[],\"unblocked_rmeta_units\":[],\
        \"sections\":null}\n];</script></body></html>";

    let input = cargo_critpath::parse(log, report).unwrap();
    assert_eq!(input.graph.len(), 2);
    // Both weights are present; which of the two identical compilations owns which is the part that
    // cannot be known.
    let mut totals: Vec<Millis> = input.graph.units().map(|(_, u)| u.total).collect();
    totals.sort();
    assert_eq!(totals, vec![Millis::new(500), Millis::new(900)]);

    let ordered: Vec<&cargo_critpath::Diagnostic> = input
        .diagnostics
        .iter()
        .filter(|d| d.code == "units-matched-by-order")
        .collect();
    assert_eq!(ordered.len(), 1);
    // One of the two, not both: once the first has taken a timing entry, the second has only one
    // candidate left and is forced rather than chosen.
    assert!(
        ordered[0].message.starts_with("1 unit(s)"),
        "{}",
        ordered[0].message
    );
}

/// One consumer naming two versions of the same package.
///
/// Cargo renames the second, so the two `--extern` arguments differ in name but agree on package.
/// Edges resolve by the artifact hash in the path rather than by that name, and this is the shape
/// that tells the two apart: a name-keyed resolver finds no producer called `thiserror_2` and
/// silently drops the edge, leaving a graph that understates the critical path.
#[test]
fn a_consumer_naming_two_versions_gets_an_edge_to_each() {
    let log = "\
     Running `/tc/bin/rustc --crate-name thiserror /reg/thiserror-1.0.69/src/lib.rs \
     --crate-type lib --emit=dep-info,metadata,link -C extra-filename=-1111111111111111 \
     --out-dir /ws/target/debug/deps`\n\
     Running `/tc/bin/rustc --crate-name thiserror /reg/thiserror-2.0.18/src/lib.rs \
     --crate-type lib --emit=dep-info,metadata,link -C extra-filename=-2222222222222222 \
     --out-dir /ws/target/debug/deps`\n\
     Running `/tc/bin/rustc --crate-name app /reg/app-0.1.0/src/lib.rs \
     --crate-type lib --emit=dep-info,metadata,link -C extra-filename=-3333333333333333 \
     --out-dir /ws/target/debug/deps \
     --extern thiserror=/ws/target/debug/deps/libthiserror-1111111111111111.rmeta \
     --extern thiserror_2=/ws/target/debug/deps/libthiserror-2222222222222222.rmeta`\n";

    let entries = [
        // Start times consistent with the graph: `app` waits for the later of the two metadata
        // moments, 0.6, so two units are live at t=0 and the header's peak of 2 is right.
        ("thiserror", "1.0.69", 0usize, 0.0_f64, 0.4_f64, 0.3_f64),
        ("thiserror", "2.0.18", 1, 0.0, 0.9, 0.6),
        ("app", "0.1.0", 2, 0.6, 0.1, 0.05),
    ]
    .iter()
    .map(|(name, version, i, start, duration, frontend)| {
        format!(
            "{{\"i\":{i},\"name\":\"{name}\",\"version\":\"{version}\",\"mode\":\"todo\",\
             \"target\":\"\",\"features\":[],\"start\":{start},\"duration\":{duration},\
             \"unblocked_units\":[],\"unblocked_rmeta_units\":[],\
             \"sections\":[[\"frontend\",{{\"start\":0.0,\"end\":{frontend}}}],\
             [\"codegen\",{{\"start\":{frontend},\"end\":{duration}}}]]}}"
        )
    })
    .collect::<Vec<String>>()
    .join(",\n");

    let timing = format!(
        "<html><body>\n<table class=\"summary-table\">\n\
         <tr>\n<td>Targets:</td><td>app 0.1.0 (lib)</td>\n</tr>\n\
         <tr>\n<td>Profile:</td><td>dev</td>\n</tr>\n\
         <tr>\n<td>Fresh units:</td><td>0</td>\n</tr>\n\
         <tr>\n<td>Dirty units:</td><td>3</td>\n</tr>\n\
         <tr>\n<td>Total units:</td><td>3</td>\n</tr>\n\
         <tr>\n<td>Max concurrency:</td><td>2 (jobs=4 ncpu=8)</td>\n</tr>\n\
         <tr>\n<td>Total time:</td><td>1.0s</td>\n</tr>\n\
         <tr>\n<td>rustc:</td><td>rustc 1.95.0 (59807616e 2026-04-14)<br>\
         Host: aarch64-apple-darwin<br>Target: aarch64-apple-darwin</td>\n</tr>\n\
         </table>\n<script>\nDURATION = 2;\nconst UNIT_DATA = [\n{entries}\n];\n\
         const CONCURRENCY_DATA = [\n{{\"t\":0.1,\"active\":2,\"waiting\":0,\"inactive\":1}}\n];\n\
         const CPU_USAGE = [\n[0.0,50.0]\n];\n</script></body></html>\n"
    );

    let input = cargo_critpath::parse(log, &timing).unwrap();
    let graph = &input.graph;
    let app = graph
        .units()
        .find(|(_, unit)| unit.package == "app")
        .map(|(id, _)| id)
        .unwrap();

    let dependencies = graph.dependencies(app).unwrap();
    assert_eq!(
        dependencies.len(),
        2,
        "both versions are named, so both are dependencies"
    );
    let mut totals: Vec<u64> = dependencies
        .iter()
        .map(|dep| graph.unit(dep.unit).unwrap().total.get())
        .collect();
    totals.sort_unstable();
    // 400ms and 900ms: two distinct units, so neither edge collapsed onto the other version.
    assert_eq!(totals, vec![400, 900]);
    for dep in dependencies {
        assert_eq!(dep.kind, EdgeKind::Metadata);
    }
    assert!(input.diagnostics.is_empty(), "{:?}", input.diagnostics);
}
