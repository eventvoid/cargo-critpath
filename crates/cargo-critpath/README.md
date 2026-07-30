# cargo-critpath

The Cargo-facing half of `cargo-critpath`: the `cargo critpath` command, and the parsers that turn
the output of a real build into the weighted dependency graph that
[`critpath-core`](https://github.com/eventvoid/cargo-critpath/tree/main/crates/critpath-core) analyses.

## The command

    cargo critpath --log build.log --timings target/cargo-timings/cargo-timing.html

`--timings` is repeatable. Giving one report per clean build of the same commit is how the
run-to-run spread of the critical path gets measured, and a saving smaller than that spread is
marked as indistinguishable from noise rather than presented as a finding. With a single report the
spread is unknown, and the report says so instead of inventing a threshold.

`--format json` writes the document specified in [`docs/json-schema.md`](https://github.com/eventvoid/cargo-critpath/blob/main/docs/json-schema.md), which is a versioned
contract with a golden test pinning its shape. `--fail-if-critical-path-over <ms>` exits 3 when the
threshold is exceeded, so a CI script can tell a regression from a tool that could not run:

    0  the analysis completed
    1  the arguments were wrong, or a path could not be read
    2  the input was rejected
    3  the analysis completed and a --fail-if threshold was exceeded

Only `cargo build` is analysed. A report from `cargo check`, `cargo test`, `cargo bench` or
`cargo doc` is refused by name, because those are different graphs rather than variations on one.
`cargo critpath --help` lists everything.

## What it reads

One command produces both inputs:

    cargo clean
    cargo build --timings -v --offline 2>build.log

- **`build.log`** — Cargo's stderr. With `-v` it holds one `` Running `…` `` line per piece of work:
  a `rustc` invocation for every compilation unit, and a bare execution of the compiled binary for
  every build script. These lines carry the unit set, the edge set, and each edge's kind.
- **`target/cargo-timings/cargo-timing.html`** — per-unit wall durations and the moment each unit
  emitted its metadata.

Optionally, and recommended for a workspace:

    cargo metadata --format-version 1 --offline --no-deps > cargo-metadata.json

Without it, a workspace-local package is identified by its crate name and then by its own directory
name. That is right for most workspaces and wrong for some — `bevy` keeps several distinct
procedural-macro packages in directories all named `macros` — and where it cannot separate two units
the parse fails and says so.

## Why both files are needed

The timing report cannot tell a plain library from a procedural macro: it renders every library
target as the empty string, and that distinction decides the kind of every edge in the graph. Its
`unblocked_units` fields are not the dependency graph either; they record only the unit that
happened to be each unit's *last* blocker, which covered 51% of the real edges on a measured
28-unit build. The verbose log has the structure and no timings. Neither is sufficient alone.

The `unblocked_*` fields are still worth having, as a check the parser did not use to build the
graph: every pair in them must be a dependency the log agreed on. `BuildInput::check_unblocked`
performs that check.

## Using it

```rust,no_run
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let outputs = cargo_critpath::collect::from_directory(std::path::Path::new("captured"))?;
let input = cargo_critpath::parse_with_metadata(
    &outputs.verbose_log,
    &outputs.timing_html,
    outputs.cargo_metadata.as_deref(),
)?;
assert!(input.check_unblocked().is_consistent());
# Ok(())
# }
```

Every parsing function takes text rather than a path, so it can be tested without a filesystem.
`collect` is the only module that touches one.

## What it refuses

A wrong graph is worse than no graph, so these are errors rather than warnings:

- a build with any fresh units, whose recorded durations are zero;
- a timing report that is truncated, has no `UNIT_DATA`, or has an entry shaped differently from
  what this parser knows;
- two inputs that describe different sets of units — every unmatched unit is named, in both
  directions;
- a `--extern` naming an artifact no invocation in the log produced, which would leave an edge out
  of the graph and understate the critical path.

## What it cannot tell you

Cargo can build one package twice at one version, for the host and for the target. Where the two
instances also share a feature set, nothing in either input separates them, and the two are paired
in the order each side lists them. They differ only in weight, and a `units-matched-by-order`
diagnostic says how many were affected. On a captured `rust-analyzer` build, 12 of 308 units were.

The Cargo version is not recorded anywhere in the timing report; `BuildMeta::cargo_version` is
therefore `None` unless a caller fills it in.
