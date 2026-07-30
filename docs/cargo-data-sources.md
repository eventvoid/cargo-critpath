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
- `target` is a display string, not structured data. The observed alphabet is `""` for a library
  target, `" build-script"` for compiling a build script, `" build-script (run)"` for running
  one, and `" name \"bin\""` for a binary. Note the leading space. A plain library and a
  proc-macro library are **indistinguishable** here — both give `""`.
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
