# Where the data comes from

Everything in this document was established by running the commands and reading the output, not
from documentation. Cargo's timing report is an undocumented internal format, so the specifics
matter and the version they were observed on is recorded.

Observed on:

    rustc 1.95.0 (59807616e 2026-04-14)   cargo 1.95.0 (f2d3ce0bd 2026-03-21)   stable
    cargo 1.99.0-nightly (3efb1f477 2026-07-17)
    aarch64-apple-darwin, 10 cores

## What is and is not available

| Command | Stable | Provides |
| --- | --- | --- |
| `cargo build --timings` | yes | unit list, per-unit wall duration and start, frontend/codegen split, build header |
| `cargo build -v` | yes | one `rustc` invocation per dirty unit: exact identity, crate type, `--emit`, and every dependency edge with its kind |
| `cargo metadata --format-version 1` | yes | package graph, target kinds, dependency kinds, resolved features |
| `cargo build --unit-graph -Zunstable-options` | no, nightly | the exact unit graph with dependency indices |
| `cargo build --timings -Zsection-timings` | no, nightly | adds `link` and `other` sections, and sections for non-pipelined units |

There is **no JSON timing output**. `--timings` is a valueless flag; `--timings=json` and
`--timings=html` are both rejected, on stable and on nightly alike:

    error: unexpected value 'json' for '--timings' found; no more were expected

So the timing parser reads HTML and extracts the JavaScript data islands the report embeds. That
is fragile by nature, which is why the parser identifies what it found and fails with a specific
message rather than a wrong number when the shape changes.

## The timings report

The HTML embeds:

    DURATION = 33.0;                            // whole-build seconds
    const UNIT_DATA = [ ... ];
    const CONCURRENCY_DATA = [ {t, active, waiting, inactive}, ... ];
    const CPU_USAGE = [ [t, percent], ... ];    // two-element arrays, not objects

plus a header table giving fresh/dirty/total unit counts, `Max concurrency`, `Total time` and the
`rustc` version.

A `UNIT_DATA` entry, verbatim:

    {"i": 16, "name": "serde_json", "version": "1.0.145", "mode": "todo",
     "target": "", "features": ["default","std"],
     "start": 1.02, "duration": 2.31,
     "unblocked_units": [], "unblocked_rmeta_units": [20],
     "sections": [["frontend", {"start": 0.0, "end": 1.83}],
                  ["codegen",  {"start": 1.83, "end": 2.31}]]}

Field by field, as measured:

- `mode` is **unusable**: it is the literal string `"todo"` for every unit except build-script
  executions, which report `"run-custom-build"`.
- `target` is a display string, not structured data, and its alphabet depends on the subcommand.
  Note the leading space on every non-empty form. Observed by running each command:

      cargo build            ""                        a library
                             " build-script"           compiling a build script
                             " build-script (run)"     running one
                             " top \"bin\""             a binary
      cargo build --release  the same four; only the header's Profile field differs
      cargo check            " (check)"                a library
                             " top \"bin\" (check)"     a binary
                             plus "" for proc-macros, which are still fully built
      cargo test --no-run    " leaf \"lib\" (test)"      a test unit
                             " top \"bin\" (test)"
                             " leaf \"lib\" (doc test)"  a doc-test unit

  A plain library and a proc-macro library are **indistinguishable** here — both give `""` — which
  is why the structure comes from the verbose log instead.

  The `check` and `test` forms were missed in the first pass of this document because only
  `cargo build` had been run. A parser written against the `build` alphabet alone does not merely
  degrade on a `cargo check` report: on ripgrep it fails to match 33 of 54 units, every library in
  the build, and produces a graph of 21 nodes whose critical path is meaningless. Which is the
  argument for enumerating an alphabet by running every command rather than the obvious one.
- `sections` times are relative to the unit's own start. On stable they are present exactly for
  pipelined units and absent otherwise. On a 28-unit build, 11 units had sections and 17 did
  not, and the 17 were precisely the 7 build-script compilations, 7 build-script executions, 2
  proc-macros and 1 binary. `frontend.end` is the metadata-emission moment.

### `unblocked_units` is not the dependency graph

The two `unblocked_*` fields record only the units for which the emitting unit happened to be the
*last remaining* blocker at run time. Compared against the true edge set on a 28-unit build:

    true edges                                35
    observed unblocked pairs                  18
    observed pairs that are not true edges     0
    true edges missing from observed          17
    coverage                                  51%

The fields are sound but half empty. A dependency of two crates reports nothing at all if some
other dependency happened to finish later in both cases. **The dependency graph cannot be
reconstructed from the timing report**, and a second input is required.

### The report's shape is not stable across Cargo versions

Three things changed within recent releases, so the parser identifies what it found and refuses
clearly rather than guessing:

- **Fresh units** were absent from `UNIT_DATA` in Cargo 1.94 and earlier, present with
  `duration: 0.0` in the 1.95 series, and filtered out again in later development. Both shapes
  have to be handled, and the header's fresh and dirty counts are the reliable signal either way.
- **The unit index `i`** corresponds to the `--unit-graph` index on Cargo 1.95 and later, because
  both are derived from the same sorted unit list. On older releases `i` was a report-local
  start-order index and the correspondence does not hold. Units are therefore matched by identity,
  and the index is used only as a corroborating check.
- **`--timings` is explicitly disclaimed upstream as not machine-readable.** Cargo's own build
  analysis work treats HTML scraping as a stopgap.

Nightly Cargo now carries `-Z build-analysis`, with `cargo report timings`, `cargo report sessions`
and `cargo report rebuilds`. It records a real edge set and per-unit freshness with rebuild
reasons, and is strictly better data than the HTML. It is nightly-only, so it is not the primary
input, but it is the obvious thing to adopt when it stabilises.

### The report cannot tell a library from a proc-macro

`UNIT_DATA`'s `target` field renders every library target as `""`. A plain rlib, a proc-macro, a
`cdylib` and a `dylib` are all indistinguishable in the timing report. Since a proc-macro consumer
needs full object code and an rlib consumer does not, this distinction decides the edge kind for
every edge in the graph — so it has to come from somewhere else. That is the second reason,
alongside the incomplete `unblocked_*` fields, that the verbose build log is the structural input.

### Fresh units poison the report

Rebuilding after touching one file:

    Fresh units: 27   Dirty units: 1   Total units: 28
    UNIT_DATA entries: 28, of which 27 have start 0.0 and duration 0.0

Cached units appear with zero duration. A timing report from an incremental build therefore
describes a graph whose weights are almost all zero, and any analysis of it is meaningless. The
header exposes the fresh and dirty counts, so this is detectable, and the tool treats it as an
input error rather than a warning.

## `cargo build -v` gives the exact graph, with edge kinds

Each dirty unit produces one `rustc` invocation. From a build of a binary depending on
serde with derive, serde\_json and thiserror:

    crate-name          crate-type    --emit                       extern kinds
    build_script_build  bin           dep-info,link                {}
    unicode_ident       lib           dep-info,metadata,link       {}
    syn                 lib           dep-info,metadata,link       {rmeta: 3}
    serde_json          lib           dep-info,metadata,link       {rmeta: 4}
    serde_derive        proc-macro    dep-info,link                {rlib: 3}
    thiserror_impl      proc-macro    dep-info,link                {rlib: 3}
    thiserror           lib           dep-info,metadata,link       {dylib: 1}
    serde               lib           dep-info,metadata,link       {rmeta: 1, dylib: 1}
    real                bin           dep-info,link                {rlib: 3}

Two rules follow, and they are exactly what the schedule model needs.

**A unit is pipelined iff its `--emit` list contains `metadata`.** Observed only for
`--crate-type lib`, never for `bin`, `proc-macro` or build scripts. Across the whole build only
two `--emit` values appeared: `dep-info,link` and `dep-info,metadata,link`.

**The extension Cargo gives the consumer states the edge kind:**

- `--extern dep=….rmeta` — a metadata edge; the consumer may start when the producer emits rmeta.
- `--extern dep=….rlib` — a full edge; the consumer waits for the producer's complete build.
- `--extern dep=….dylib` — a full edge; the producer is a proc-macro and must be linked and
  loadable.

`serde`'s mixed `{rmeta: 1, dylib: 1}` is the model in one line: `serde_core` reached through
metadata, the `serde_derive` proc-macro only when fully built. And `serde_derive`, itself a
proc-macro, receives its own ordinary library dependencies as `.rlib` — it is the *consumer's*
nature that forces full object code.

`-C metadata=<hash>` and `-C extra-filename=<hash>` give each unit a stable identity, and each
`--extern` path embeds the producer's `extra-filename`. Edges therefore resolve by hash rather
than by name, and no part of Cargo's unit-construction logic has to be guessed at.

`-v` prints `rustc` lines only for dirty units, which composes with the fresh-unit problem above:
a full, clean build is required either way.

### Build-script executions are in the log too

Build-script *executions* are not confined to the timing report. The verbose log records each one as
a bare execution of the compiled script,

    Running `/…/target/debug/build/serde_core-c9c71f116f26aaba/build-script-build`

with no `rustc` and no arguments. Three consequences, and they simplify everything:

- the `Running` lines are **one-to-one** with `UNIT_DATA` — verified 54/54, 119/119, 308/308 and
  419/419 on the four measured projects;
- the compile-to-run edge is *stated* rather than synthesised from Cargo's three-unit pattern;
- an execution inherits its compilation's package, version and features through the hash in that
  path, so it needs no separate attribution.

A build-script **compilation** is likewise identified exactly, by its `--out-dir`: Cargo places build
scripts under `target/<profile>/build/<package>-<hash>` and everything else under `deps`, so the
directory both marks it as a build script and names its package, workspace-local or not. That is
worth more than `--crate-name`, which is only `build_script_build` when the file is called
`build.rs`.

### Other details, each of which will produce a wrong answer if missed

- **`UNIT_DATA`'s `i` is not the array index.** The array is in start order; `i` is a permutation
  matching Cargo's own sorted unit list, and `unblocked_units` refers to `i`. Indexing the array by
  it silently misattributes every pair.
- **`sections` is `null`, not `[]`,** for a unit that is not pipelined.
- **`Total time` is not always `N.Ns`.** Over a minute it reads `61.2s (1m 1.2s)`.
- **`DURATION` is not the build's total.** It is the chart's axis ceiling: a build whose header says
  `Total time: 6.1s` carries `DURATION = 7;`.
- **`--crate-type bin` is written explicitly**, so "no `--crate-type` means a binary" is necessary
  but not sufficient, and treating its absence as the only signal loses every binary in the build.
- **`--extern proc_macro` has no `=path`** and is not an edge.
- **`Max concurrency` holds three numbers**, as in `12 (jobs=10 ncpu=10)`.
- **The header does not contain Cargo's version**, only rustc's.

## Cargo's scheduler, and what it means for prediction

Read from Cargo's source, at commit `0d83fa6`:

- Every unit is queued with a **hard-coded cost of 100**. Cargo's scheduler has no idea how long
  anything takes.
- A unit's priority is its own cost plus the cost of all of its transitive reverse-dependencies,
  which reduces to `1 + (number of transitive reverse-dependents)`.
- Having found the highest priority, Cargo breaks ties by **hash-map iteration order**.

The last point puts a floor on how accurately any simulator can predict a single build: two runs
of the same build can legitimately schedule differently. It is also why the simulator offers a
`cargo-like` policy rather than only a smarter one — reproducing a cost-blind scheduler is the only
way to estimate Cargo's wall time instead of the wall time of a better build system.

## `-j` is not a count of compiler processes

Cargo runs a jobserver. A `rustc` process holds one implicit token for its lifetime and can
acquire further tokens for parallel codegen. So `-j N` bounds the token pool, not the number of
concurrent compilations, and "N executors" is an upper bound on concurrency rather than a
description of it.

Two effects pull in opposite directions, and both were measured, so the honest statement is that
the net direction is not known in general.

*Against the model's optimism:* a `rustc` can consume more than one token, so fewer compilations
run at once than `J` suggests, which makes a simulated makespan too short.

*Against the model's pessimism:* observed concurrency can **exceed** `-j`. A clean `ripgrep` build
at `-j 10` reported `Max concurrency: 12 (jobs=10 ncpu=10)`, and 7 of its 83 concurrency samples
were above 10. Inspecting one of them, 11 units were active and 5 of those were build-script
*executions*, which are not compiler processes and do not consume a compile token:

    t=0.64  active=11  waiting=1
      memchr (lib), regex-syntax (lib), encoding_rs (lib), aho-corasick (lib), grep-matcher (lib)
      serde (build-script compile)
      serde_json, serde_core, zmij, crossbeam-utils, libc  (build-script runs)

Six compilations and five build-script runs. Capping the simulation at `J` therefore holds back
work that Cargo would have run, which makes a simulated makespan too long.

So `J` is neither a floor nor a ceiling on real concurrency. The tool reports the observed
`Max concurrency` from the build alongside its own `J` so that the discrepancy is visible rather
than assumed away.

## The model predicts the observed timeline

On a six-crate workspace built with a leaf library, two middle libraries, a top binary, a
proc-macro and a build script:

    bs (lib)   start 0.84, frontend ends at +0.03  -> metadata available at 0.87
    midB       start 0.87    matches the metadata edge exactly
                             (full completion would have been 0.88)
    midB       ends 0.90
    top (bin)  start 0.90    matches the full edge, since a binary must link
    midA       ends 0.58, yet top waited for midB — consistent

Both edge kinds reproduce the observed start times to the resolution of the data.

## Time that is in the wall clock but not in the units

First clean build of the same project with a cold registry:

    Total time 32.5s      earliest unit start 27.96s      last unit end 32.73s

Only about 4.7 s of 32.5 s was compilation; the rest was Cargo's network phase. With a warm cache
but a cleaned target directory the prefix was still 8.06 s, and `--offline` collapsed it:

    --offline:  wall 5.39s   earliest unit start 0.29s

The prefix is not recoverable from `UNIT_DATA` — the units simply start late on a timeline whose
origin precedes the fetch. So predictions are compared against the **compilation span**
(last unit end minus first unit start) rather than against Cargo's reported total, and the
excluded prefix is reported separately. Data collection uses `--offline` where it can.

## Inputs the tool uses

Structure and weights come from different places, so both are taken from one build:

    cargo build --timings -v --offline

- structure and edge kinds from the verbose `rustc` invocations, on stable, exactly;
- weights and the metadata split from the timing report;
- package and feature context from `cargo metadata`, needed for the drop-dependency and
  drop-feature counterfactuals, which are package-level questions.

`--unit-graph` is supported as an optional higher-fidelity structure source for anyone on
nightly, and is used in the test suite as an oracle against the verbose-output parser.

Because both required inputs come from a single build, the tool never asks for a matching pair of
commands and never has to reconcile two different builds.
