//! What the verbose build log must yield.
//!
//! Every expected value here is read off the log text in the test itself, not produced by the code
//! under test. The log lines are shortened copies of real `cargo build -v` output with the machine's
//! paths replaced by a placeholder root.

// A test asserting a parse result is allowed to unwrap: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use cargo_critpath::{ParseError, Role};
use critpath_core::{EdgeKind, UnitKind};

const REGISTRY: &str = "/reg/index.crates.io-1949cf8c6b5b557f";

fn log() -> String {
    format!(
        "   Compiling unicode-ident v1.0.20\n\
            Compiling libc v0.2.186\n\
             Running `/tc/bin/rustc --crate-name unicode_ident --edition=2021 \
             {REGISTRY}/unicode-ident-1.0.20/src/lib.rs --crate-type lib \
             --emit=dep-info,metadata,link -C metadata=aaaa0000aaaa0000 \
             -C extra-filename=-1111111111111111 --out-dir /ws/target/debug/deps`\n\
             Running `/tc/bin/rustc --crate-name build_script_build --edition=2021 \
             {REGISTRY}/libc-0.2.186/build.rs --crate-type bin --emit=dep-info,link \
             -C metadata=bbbb0000bbbb0000 -C extra-filename=-2222222222222222 \
             --out-dir /ws/target/debug/build/libc-2222222222222222`\n\
             Running `/ws/target/debug/build/libc-2222222222222222/build-script-build`\n\
             Running `/tc/bin/rustc --crate-name syn --edition=2021 \
             {REGISTRY}/syn-2.0.90/src/lib.rs --crate-type lib \
             --emit=dep-info,metadata,link --cfg 'feature=\"full\"' --cfg 'feature=\"default\"' \
             --check-cfg 'cfg(docsrs,test)' -C metadata=cccc0000cccc0000 \
             -C extra-filename=-3333333333333333 --out-dir /ws/target/debug/deps \
             --extern unicode_ident={REGISTRY}/../deps/libunicode_ident-1111111111111111.rmeta`\n\
             Running `/tc/bin/rustc --crate-name serde_derive --edition=2021 \
             {REGISTRY}/serde_derive-1.0.228/src/lib.rs --crate-type proc-macro \
             --emit=dep-info,link -C prefer-dynamic -C metadata=dddd0000dddd0000 \
             -C extra-filename=-4444444444444444 --out-dir /ws/target/debug/deps \
             --extern syn=/ws/target/debug/deps/libsyn-3333333333333333.rlib \
             --extern proc_macro`\n\
             Running `/tc/bin/rustc --crate-name demo --edition=2021 src/main.rs \
             --crate-type bin --emit=dep-info,link -C metadata=eeee0000eeee0000 \
             -C extra-filename=-5555555555555555 --out-dir /ws/target/debug/deps \
             --extern serde_derive=/ws/target/debug/deps/libserde_derive-4444444444444444.dylib`\n"
    )
}

#[test]
fn every_running_line_becomes_one_invocation() {
    let parsed = cargo_critpath::parse_verbose_log(&log()).unwrap();
    // Six `Running` lines: five rustc invocations and one build-script execution.
    assert_eq!(parsed.invocations.len(), 6);
    assert!(parsed.unrecognised.is_empty());
    let roles: Vec<Role> = parsed.invocations.iter().map(|i| i.role).collect();
    assert_eq!(
        roles,
        vec![
            Role::Compile,
            Role::Compile,
            Role::BuildScriptRun,
            Role::Compile,
            Role::Compile,
            Role::Compile,
        ]
    );
}

#[test]
fn a_unit_is_pipelined_exactly_when_it_emits_metadata() {
    let parsed = cargo_critpath::parse_verbose_log(&log()).unwrap();
    let pipelined: Vec<(&str, bool)> = parsed
        .invocations
        .iter()
        .filter(|i| i.role == Role::Compile)
        .map(|i| (i.crate_name.as_str(), i.pipelined))
        .collect();
    assert_eq!(
        pipelined,
        vec![
            ("unicode_ident", true),
            ("build_script_build", false),
            ("syn", true),
            ("serde_derive", false),
            ("demo", false),
        ]
    );
}

#[test]
fn the_extern_extension_decides_the_edge_kind() {
    let parsed = cargo_critpath::parse_verbose_log(&log()).unwrap();
    let syn = parsed
        .invocations
        .iter()
        .find(|i| i.crate_name == "syn")
        .unwrap();
    assert_eq!(syn.externs.len(), 1);
    assert_eq!(syn.externs[0].name, "unicode_ident");
    assert_eq!(syn.externs[0].edge, EdgeKind::Metadata);
    assert_eq!(
        syn.externs[0].producer.as_deref(),
        Some("-1111111111111111")
    );

    let derive = parsed
        .invocations
        .iter()
        .find(|i| i.crate_name == "serde_derive")
        .unwrap();
    // `--extern proc_macro` has no path and names the compiler's own crate, so it is not an edge.
    assert_eq!(derive.externs.len(), 1);
    assert_eq!(derive.externs[0].edge, EdgeKind::Full);

    let demo = parsed
        .invocations
        .iter()
        .find(|i| i.crate_name == "demo")
        .unwrap();
    assert_eq!(demo.externs[0].edge, EdgeKind::Full);
    assert_eq!(demo.externs[0].extension, "dylib");
}

#[test]
fn kinds_and_closure_requirements_follow_cargos_own_rule() {
    let parsed = cargo_critpath::parse_verbose_log(&log()).unwrap();
    let seen: Vec<(&str, UnitKind, bool)> = parsed
        .invocations
        .iter()
        .map(|i| (i.crate_name.as_str(), i.kind, i.requires_full_closure))
        .collect();
    // Cargo's rule is `mode.is_any_test() || !matches!(crate_type, Lib | Rlib)`, so a plain library
    // is the only thing that does not need its dependencies' object code.
    assert_eq!(
        seen,
        vec![
            ("unicode_ident", UnitKind::Lib, false),
            ("build_script_build", UnitKind::BuildScriptCompile, true),
            ("", UnitKind::BuildScriptRun, false),
            ("syn", UnitKind::Lib, false),
            ("serde_derive", UnitKind::ProcMacro, true),
            ("demo", UnitKind::Bin, true),
        ]
    );
}

#[test]
fn a_registry_source_path_names_the_package_and_version() {
    let parsed = cargo_critpath::parse_verbose_log(&log()).unwrap();
    let syn = parsed
        .invocations
        .iter()
        .find(|i| i.crate_name == "syn")
        .unwrap();
    let package = syn.registry_package.as_ref().unwrap();
    assert_eq!(package.name, "syn");
    assert_eq!(package.version, "2.0.90");

    // A workspace-local crate has no `name-version/` component, so nothing is claimed.
    let demo = parsed
        .invocations
        .iter()
        .find(|i| i.crate_name == "demo")
        .unwrap();
    assert!(demo.registry_package.is_none());
    assert_eq!(demo.source_path, "src/main.rs");
}

#[test]
fn a_build_script_execution_names_its_package_and_its_compiled_artifact() {
    let parsed = cargo_critpath::parse_verbose_log(&log()).unwrap();
    let run = parsed
        .invocations
        .iter()
        .find(|i| i.role == Role::BuildScriptRun)
        .unwrap();
    // `.../build/libc-2222222222222222/build-script-build`: the directory carries the package name
    // and the compiled script's own `-C extra-filename`, which is how the run is tied to the
    // compile without guessing.
    assert_eq!(run.run_package.as_deref(), Some("libc"));
    assert_eq!(run.run_artifact.as_deref(), Some("-2222222222222222"));
}

#[test]
fn features_are_recovered_from_cfg_flags_and_sorted() {
    let parsed = cargo_critpath::parse_verbose_log(&log()).unwrap();
    let syn = parsed
        .invocations
        .iter()
        .find(|i| i.crate_name == "syn")
        .unwrap();
    // `--check-cfg 'cfg(docsrs,test)'` is not a feature and must not be collected.
    assert_eq!(syn.features, vec!["default".to_owned(), "full".to_owned()]);
}

#[test]
fn quoting_and_crlf_are_handled() {
    let text = "     Running `/tc/bin/rustc --crate-name a 'src/lib.rs' --crate-type lib \
                --emit=dep-info,metadata,link --cfg 'feature=\"x y\"' \
                -C extra-filename=-9999999999999999`\r\n";
    let parsed = cargo_critpath::parse_verbose_log(text).unwrap();
    assert_eq!(parsed.invocations.len(), 1);
    assert_eq!(parsed.invocations[0].source_path, "src/lib.rs");
    assert_eq!(parsed.invocations[0].features, vec!["x y".to_owned()]);
}

#[test]
fn a_log_with_no_invocations_is_refused_with_advice() {
    let err = cargo_critpath::parse_verbose_log("   Compiling libc v0.2.186\n").unwrap_err();
    assert!(matches!(err, ParseError::NoInvocations));
    let message = err.to_string();
    assert!(message.contains("cargo build --timings -v"), "{message}");
}

#[test]
fn an_unrecognised_running_line_is_kept_rather_than_dropped() {
    let text = "     Running `/tc/bin/rustdoc --crate-name a src/lib.rs`\n     \
                Running `/tc/bin/rustc --crate-name a src/lib.rs --crate-type lib \
                --emit=dep-info,link -C extra-filename=-8888888888888888`\n";
    let parsed = cargo_critpath::parse_verbose_log(text).unwrap();
    assert_eq!(parsed.invocations.len(), 1);
    assert_eq!(parsed.unrecognised.len(), 1);
    assert!(parsed.unrecognised[0].contains("rustdoc"));
}
