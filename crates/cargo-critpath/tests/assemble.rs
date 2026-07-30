//! Matching the two inputs to each other, and the graph that results.
//!
//! The fixtures under `tests/fixtures/workspace` are the real output of one
//! `cargo build --timings -v --offline -j 4` of a four-crate workspace containing a build script, a
//! procedural macro and a registry dependency, with the machine's paths replaced. Every expected
//! number below was read off those two files by hand.

// A test asserting a parse result is allowed to unwrap: a panic is the failure report.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use cargo_critpath::{BuildInput, ParseError};
use critpath_core::{EdgeKind, Millis, UnitId, UnitKind};

const VERBOSE: &str = include_str!("fixtures/workspace/verbose.log");
const TIMING: &str = include_str!("fixtures/workspace/cargo-timing.html");
const METADATA: &str = include_str!("fixtures/workspace/cargo-metadata.json");
const INCREMENTAL_TIMING: &str = include_str!("fixtures/incremental/cargo-timing.html");
const INCREMENTAL_VERBOSE: &str = include_str!("fixtures/incremental/verbose.log");
const TRUNCATED: &str = include_str!("fixtures/truncated/cargo-timing.html");

fn input() -> BuildInput {
    cargo_critpath::parse(VERBOSE, TIMING).unwrap()
}

/// The identifier of the unit with this package and target, which is unique in this fixture.
fn unit_of(input: &BuildInput, package: &str, target: &str) -> UnitId {
    input
        .graph
        .units()
        .find(|(_, u)| u.package == package && u.target == target)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no unit for {package} ({target})"))
}

fn edge(input: &BuildInput, from: UnitId, to: UnitId) -> Option<EdgeKind> {
    input
        .graph
        .dependencies(to)?
        .iter()
        .find(|d| d.unit == from)
        .map(|d| d.kind)
}

#[test]
fn the_workspace_build_yields_nine_units_and_nine_edges() {
    let input = input();
    // Nine `Running` lines and nine `UNIT_DATA` entries: two build-script compilations, two
    // build-script executions, three libraries, one procedural macro and one binary.
    assert_eq!(input.graph.len(), 9);
    assert_eq!(input.graph.edge_count(), 9);
    // 90 + 150 + 400 + 690 + 40 + 150 + 520 + 280 + 110 ms, summed from `UNIT_DATA` by hand.
    assert_eq!(input.graph.total_work(), Millis::new(2_430));
}

#[test]
fn units_are_inserted_in_a_canonical_order_so_the_graph_is_reproducible() {
    let input = input();
    let order: Vec<(String, String)> = input
        .graph
        .units()
        .map(|(_, u)| (u.package.clone(), u.target.clone()))
        .collect();
    assert_eq!(
        order,
        vec![
            ("app".to_owned(), "demo".to_owned()),
            ("hello-macros".to_owned(), "lib".to_owned()),
            ("leaf".to_owned(), "build-script".to_owned()),
            ("leaf".to_owned(), "build-script (run)".to_owned()),
            ("leaf".to_owned(), "lib".to_owned()),
            ("libc".to_owned(), "build-script".to_owned()),
            ("libc".to_owned(), "build-script (run)".to_owned()),
            ("libc".to_owned(), "lib".to_owned()),
            ("mid".to_owned(), "lib".to_owned()),
        ]
    );
    // The same input twice gives the same graph, whatever the map iteration order of the day.
    let again = input3();
    let repeated: Vec<(String, String)> = again
        .graph
        .units()
        .map(|(_, u)| (u.package.clone(), u.target.clone()))
        .collect();
    assert_eq!(order, repeated);
}

fn input3() -> BuildInput {
    cargo_critpath::parse(VERBOSE, TIMING).unwrap()
}

#[test]
fn a_plain_library_dependency_is_a_metadata_edge() {
    let input = input();
    let leaf = unit_of(&input, "leaf", "lib");
    let mid = unit_of(&input, "mid", "lib");
    // `--extern leaf=….rmeta`: mid may start once leaf has emitted metadata.
    assert_eq!(edge(&input, leaf, mid), Some(EdgeKind::Metadata));
    // leaf's frontend ended at 0.03 s of its 0.04 s, so 30 ms of its 40 ms is what mid waits for.
    let leaf_unit = input.graph.unit(leaf).unwrap();
    assert_eq!(leaf_unit.total, Millis::new(40));
    assert_eq!(leaf_unit.metadata_at, Some(Millis::new(30)));
}

#[test]
fn a_procedural_macro_makes_its_dependents_wait_for_all_of_it() {
    let input = input();
    let leaf = unit_of(&input, "leaf", "lib");
    let macros = unit_of(&input, "hello-macros", "lib");
    let mid = unit_of(&input, "mid", "lib");

    // The macro is handed leaf as `.rlib`, not `.rmeta`: a proc-macro is linked and loaded, so it
    // needs object code even from a dependency an ordinary library would reach through metadata.
    assert_eq!(edge(&input, leaf, macros), Some(EdgeKind::Full));
    // And mid is handed the macro as `.dylib`, which is also a complete build.
    assert_eq!(edge(&input, macros, mid), Some(EdgeKind::Full));

    let macro_unit = input.graph.unit(macros).unwrap();
    assert_eq!(macro_unit.kind, UnitKind::ProcMacro);
    assert!(macro_unit.requires_full_closure);
    // It emits no metadata, so nothing can start against it early.
    assert_eq!(macro_unit.metadata_at, None);
}

#[test]
fn the_binary_needs_its_whole_closure() {
    let input = input();
    let demo = input.graph.unit(unit_of(&input, "app", "demo")).unwrap();
    assert_eq!(demo.kind, UnitKind::Bin);
    // A linker needs every object file in the closure, and pipelining does not guarantee they
    // exist: an rlib can be on disk while an rlib it depends on is not.
    assert!(demo.requires_full_closure);
    assert_eq!(demo.metadata_at, None);

    let leaf = input.graph.unit(unit_of(&input, "leaf", "lib")).unwrap();
    assert!(!leaf.requires_full_closure);
}

#[test]
fn the_build_script_triple_is_wired_compile_then_run_then_the_package() {
    let input = input();
    for package in ["leaf", "libc"] {
        let compile = unit_of(&input, package, "build-script");
        let run = unit_of(&input, package, "build-script (run)");
        let lib = unit_of(&input, package, "lib");
        // Neither of these edges is named by any `--extern`; they are Cargo's three-unit pattern,
        // reconstructed from the `build/<package>-<hash>` directory the run executes from.
        assert_eq!(
            edge(&input, compile, run),
            Some(EdgeKind::Full),
            "{package}"
        );
        assert_eq!(edge(&input, run, lib), Some(EdgeKind::Full), "{package}");
        // And not the other way round.
        assert_eq!(edge(&input, run, compile), None, "{package}");

        let compile_unit = input.graph.unit(compile).unwrap();
        assert_eq!(compile_unit.kind, UnitKind::BuildScriptCompile);
        assert!(compile_unit.requires_full_closure);
        let run_unit = input.graph.unit(run).unwrap();
        assert_eq!(run_unit.kind, UnitKind::BuildScriptRun);
        // Running a program links nothing, and its executable already waited for its own closure.
        assert!(!run_unit.requires_full_closure);
    }
}

#[test]
fn the_build_context_is_carried_through() {
    let input = input();
    let build = &input.build;
    assert_eq!(build.jobs, Some(4));
    assert_eq!(build.observed_max_concurrency, Some(3));
    assert_eq!(build.profile.as_deref(), Some("dev"));
    assert_eq!(build.units_total, 9);
    assert_eq!(build.units_fresh, 0);
    // First unit starts at 0.10 s; mid ends at 0.78 + 0.52 = 1.30 s and the binary at 1.41 s.
    assert_eq!(build.compilation_span, Millis::new(1_310));
    assert_eq!(build.reported_total, Some(Millis::new(1_400)));
    // 1.40 s reported against 1.31 s of compilation, so 90 ms went somewhere the model cannot see.
    assert_eq!(build.excluded_prefix, Millis::new(90));
    assert!(build.host.is_some());
    assert!(build.rustc_version.is_some());
    // The report's header states no Cargo version, so nothing is claimed for it.
    assert_eq!(build.cargo_version, None);
}

#[test]
fn every_unblocked_pair_the_report_recorded_is_an_edge_the_log_agreed_on() {
    let input = input();
    let check = input.check_unblocked();
    // Seven pairs across nine units: the fields record only each unit's last blocker, so they are
    // far from the nine edges. Every one of them still has to be an edge.
    assert_eq!(check.total, 7);
    assert_eq!(check.confirmed, 7);
    assert!(check.unconfirmed.is_empty(), "{:?}", check.unconfirmed);
}

#[test]
fn cargo_metadata_and_the_directory_fallback_agree_on_this_workspace() {
    let with = cargo_critpath::parse_with_metadata(VERBOSE, TIMING, Some(METADATA)).unwrap();
    let without = input();
    let labels = |input: &BuildInput| -> Vec<String> {
        input
            .graph
            .units()
            .map(|(_, u)| format!("{} {} {}", u.package, u.version, u.target))
            .collect()
    };
    assert_eq!(labels(&with), labels(&without));
    assert_eq!(with.graph.edge_count(), without.graph.edge_count());
}

#[test]
fn a_missing_invocation_is_a_refusal_rather_than_a_smaller_graph() {
    // Drop the procedural macro's compilation from the log, as an interrupted capture would.
    let mutilated: String = VERBOSE
        .lines()
        .filter(|line| !line.contains("--crate-name hello_macros"))
        .map(|line| format!("{line}\n"))
        .collect();
    let err = cargo_critpath::parse(&mutilated, TIMING).unwrap_err();
    match err {
        ParseError::UnmatchedUnits {
            ref in_log,
            ref in_report,
            had_metadata,
        } => {
            assert!(in_log.is_empty(), "{in_log:?}");
            assert_eq!(in_report.len(), 1);
            assert_eq!(in_report[0].package, "hello-macros");
            assert!(!had_metadata);
        }
        other => panic!("expected unmatched units, got {other}"),
    }
    // The message has to name the units and say what to do, because a graph missing one unit
    // understates the critical path without looking wrong.
    let message = err.to_string();
    assert!(message.contains("hello-macros"), "{message}");
    assert!(message.contains("cargo metadata"), "{message}");
}

#[test]
fn an_incremental_build_is_refused_before_any_graph_is_built() {
    let err = cargo_critpath::parse(INCREMENTAL_VERBOSE, INCREMENTAL_TIMING).unwrap_err();
    // Touching one file left seven of nine units cached.
    assert_eq!(err, ParseError::FreshUnits { fresh: 7, total: 9 });
}

#[test]
fn a_truncated_report_is_refused() {
    assert_eq!(
        cargo_critpath::parse(VERBOSE, TRUNCATED).unwrap_err(),
        ParseError::TruncatedArray {
            name: "UNIT_DATA".to_owned()
        }
    );
}

#[test]
fn an_empty_log_and_an_empty_report_do_not_panic() {
    assert_eq!(
        cargo_critpath::parse("", TIMING).unwrap_err(),
        ParseError::NoInvocations
    );
    assert_eq!(
        cargo_critpath::parse(VERBOSE, "").unwrap_err(),
        ParseError::NoUnitData
    );
}

#[test]
fn metadata_that_is_not_metadata_is_named_as_such() {
    let err = cargo_critpath::parse_with_metadata(VERBOSE, TIMING, Some("{oh no")).unwrap_err();
    assert!(matches!(err, ParseError::Metadata { .. }), "{err}");
    assert!(err.to_string().contains("cargo metadata"), "{err}");
}

/// A build whose critical path runs *through* metadata edges.
///
/// The workspace fixture's only metadata edge is off its critical path, so every step there
/// contributes its whole duration and nothing end to end distinguishes crediting pipelining from
/// ignoring it. Pipelining is the model's central claim, so it is asserted here on a graph built for
/// the purpose.
mod pipelined {
    use super::*;

    /// `a` and `b` take a second each and emit metadata early; `c` is off the path; `app` links.
    /// `d` emits no metadata at all despite the report giving it a frontend section.
    fn log() -> String {
        let mut text = String::from("   Compiling a v1.0.0\n");
        for (name, extra, hash, emit, kind, extern_arg) in [
            (
                "a",
                "-1111111111111111",
                "aaaa0000aaaa0001",
                "dep-info,metadata,link",
                "lib",
                "",
            ),
            (
                "b",
                "-2222222222222222",
                "bbbb0000bbbb0002",
                "dep-info,metadata,link",
                "lib",
                "--extern a=/ws/target/debug/deps/liba-1111111111111111.rmeta",
            ),
            (
                "c",
                "-3333333333333333",
                "cccc0000cccc0003",
                "dep-info,metadata,link",
                "lib",
                "--extern b=/ws/target/debug/deps/libb-2222222222222222.rmeta",
            ),
            (
                "app",
                "-4444444444444444",
                "dddd0000dddd0004",
                "dep-info,link",
                "bin",
                "--extern c=/ws/target/debug/deps/libc-3333333333333333.rlib",
            ),
            (
                "d",
                "-5555555555555555",
                "eeee0000eeee0005",
                "dep-info,link",
                "proc-macro",
                "",
            ),
        ] {
            let source = if name == "app" { "main.rs" } else { "lib.rs" };
            text.push_str(&format!(
                " Running `/tc/bin/rustc --crate-name {name} --edition=2021 \
                 /reg/{name}-1.0.0/src/{source} --crate-type {kind} --emit={emit} \
                 -C metadata={hash} -C extra-filename={extra} \
                 --out-dir /ws/target/debug/deps {extern_arg}`\n"
            ));
        }
        text
    }

    fn timing() -> String {
        // durations in seconds, and where the frontend section ends
        let units = [
            (0, "a", "", 0.0, 1.0, Some(0.2)),
            (1, "b", "", 0.2, 1.0, Some(0.3)),
            (2, "c", "", 0.5, 0.5, Some(0.4)),
            (3, "app", " app \\\"bin\\\"", 1.2, 0.1, None),
            // A frontend section, yet `--emit` never mentioned metadata: nothing may wait on it.
            (4, "d", "", 0.0, 0.6, Some(0.5)),
        ];
        let entries: Vec<String> = units
            .iter()
            .map(|(i, name, target, start, duration, frontend)| {
                let sections = match frontend {
                    Some(end) => format!(
                        "[[\"frontend\",{{\"start\":0.0,\"end\":{end}}}],\
                         [\"codegen\",{{\"start\":{end},\"end\":{duration}}}]]"
                    ),
                    // Cargo writes `null`, not an empty list, for a unit that is not pipelined.
                    None => "null".to_owned(),
                };
                format!(
                    "{{\"i\":{i},\"name\":\"{name}\",\"version\":\"1.0.0\",\"mode\":\"todo\",\
                     \"target\":\"{target}\",\"features\":[],\"start\":{start},\
                     \"duration\":{duration},\"unblocked_units\":[],\
                     \"unblocked_rmeta_units\":[],\"sections\":{sections}}}"
                )
            })
            .collect();
        format!(
            "<html><body>\n<table class=\"summary-table\">\n\
             <tr>\n<td>Targets:</td><td>app 1.0.0 ( app \"bin\")</td>\n</tr>\n\
             <tr>\n<td>Profile:</td><td>dev</td>\n</tr>\n\
             <tr>\n<td>Fresh units:</td><td>0</td>\n</tr>\n\
             <tr>\n<td>Dirty units:</td><td>5</td>\n</tr>\n\
             <tr>\n<td>Total units:</td><td>5</td>\n</tr>\n\
             <tr>\n<td>Max concurrency:</td><td>3 (jobs=4 ncpu=8)</td>\n</tr>\n\
             <tr>\n<td>Total time:</td><td>1.3s</td>\n</tr>\n\
             <tr>\n<td>rustc:</td><td>rustc 1.95.0 (59807616e 2026-04-14)<br>\
             Host: aarch64-apple-darwin<br>Target: aarch64-apple-darwin</td>\n</tr>\n\
             </table>\n<script>\nDURATION = 2;\nconst UNIT_DATA = [\n{}\n];\n\
             const CONCURRENCY_DATA = [\n{{\"t\":0.1,\"active\":2,\"waiting\":0,\"inactive\":3}}\n];\n\
             const CPU_USAGE = [\n[0.0,50.0]\n];\n</script></body></html>\n",
            entries.join(",\n")
        )
    }

    #[test]
    fn the_critical_path_credits_metadata_rather_than_whole_durations() {
        let input = cargo_critpath::parse(&log(), &timing()).unwrap();
        let graph = &input.graph;
        let analysis = critpath_core::analyze(graph);

        let named = |package: &str| {
            graph
                .units()
                .find(|(_, unit)| unit.package == package)
                .map(|(id, _)| id)
                .unwrap()
        };
        let (a, b, c, app) = (named("a"), named("b"), named("c"), named("app"));

        // Hand-computed. `a` runs 0..1000 and emits metadata at 200, so `b` starts at 200 and ends
        // at 1200, emitting its own metadata at 500. `c` starts at 500 and ends at 1000. `app`
        // links, so it waits for its whole closure to finish, which is `b` at 1200, and ends at
        // 1300.
        assert_eq!(analysis.earliest_start(b), Some(Millis::new(200)));
        assert_eq!(analysis.earliest_start(c), Some(Millis::new(500)));
        assert_eq!(analysis.earliest_start(app), Some(Millis::new(1_200)));
        assert_eq!(analysis.critical_path_len(), Millis::new(1_300));

        // Summing the whole durations along the path would give 1000 + 1000 + 100 = 2100. The
        // critical path is 1300 because `a` only has to reach its metadata before `b` may start.
        assert_eq!(analysis.critical_path(), &[a, b, app]);
        assert_eq!(analysis.critical_contribution(a), Some(Millis::new(200)));
        assert_eq!(analysis.critical_contribution(b), Some(Millis::new(1_000)));
        assert_eq!(analysis.critical_contribution(app), Some(Millis::new(100)));
        // Pinned so that the comparison above is a statement about pipelining and not about
        // whatever `a`'s duration happened to parse as: 200 of 1000, and the durations along the
        // path total 2100 against a critical path of 1300.
        assert_eq!(graph.unit(a).unwrap().total, Millis::new(1_000));
        assert_eq!(graph.unit(b).unwrap().total, Millis::new(1_000));
        assert_eq!(graph.unit(app).unwrap().total, Millis::new(100));

        // The identity this fixture exists to make non-vacuous.
        let summed = [a, b, app]
            .iter()
            .map(|id| analysis.critical_contribution(*id).unwrap())
            .fold(Millis::ZERO, Millis::saturating_add);
        assert_eq!(summed, analysis.critical_path_len());

        // `c` is not on the path at all: it finishes at 1000 while `b` finishes at 1200.
        assert_eq!(analysis.critical_contribution(c), None);
        assert!(analysis.slack(c).unwrap() > Millis::ZERO);

        // A unit whose `--emit` omitted metadata emits none, whatever sections the report gives it.
        let d = named("d");
        assert_eq!(graph.unit(d).unwrap().metadata_at, None);
        assert_eq!(graph.unit(a).unwrap().metadata_at, Some(Millis::new(200)));
        assert!(graph.unit(app).unwrap().requires_full_closure);
    }
}
