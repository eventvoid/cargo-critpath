# Fixtures

Real output of real builds, with the machine's paths replaced by placeholders. Nothing here was
written by hand, and nothing here was edited except as described below.

## `workspace/`

One `cargo build --timings -v --offline -j 4` of a four-crate workspace built for this purpose:

    leaf          library with a build script and two features
    hello-macros  procedural macro, depends on leaf
    mid           library, depends on leaf and on hello-macros
    app           binary `demo`, depends on mid and on the registry crate libc

Nine units: two build-script compilations, two build-script executions, three libraries, one
procedural macro and one binary. It exercises every relationship the adapter has to get right — a
metadata edge, a procedural macro's dependents waiting for all of it, a binary needing its whole
closure, and Cargo's three-unit build-script pattern — in a graph small enough to check by hand.

- `verbose.log` — Cargo's stderr.
- `cargo-timing.html` — `target/cargo-timings/cargo-timing.html`, complete.
- `cargo-metadata.json` — `cargo metadata --format-version 1 --offline --no-deps`.

### `workspace/cargo-timing-slower.html` is derived, not captured

This one file is **not** the output of a second build. It is `workspace/cargo-timing.html` with
every `start`, every `duration`, every section boundary and the `Total time` row multiplied by 1.10
and rounded to the two decimal places Cargo writes. Everything else, including the unit set, the
feature sets and the `unblocked_*` fields, is unchanged.

It exists because the run-to-run spread of the critical path needs at least two reports of the same
build to be measurable at all, and only one build of this workspace was captured. A uniform 10%
stretch is a plausible repeat — the measured spread on real projects runs from 2.3% to 11.8% — but
it is a stretch of one measurement rather than a second measurement, and no figure derived from it
should be quoted as evidence about real builds. It is test input for the code path, nothing more.

## `incremental/`

The same workspace rebuilt after touching one file, so seven of the nine units were reused. Its
recorded durations are almost all zero, which is why the analysis refuses it.

## `truncated/`

`workspace/cargo-timing.html` cut off in the middle of `UNIT_DATA`, which is what an interrupted
build leaves behind.

## Sanitisation

Every file had these substitutions applied, and nothing else:

    <the temporary build directory>          ->  /workspace
    <the rustup toolchain directory>         ->  /toolchain
    <the cargo home directory>               ->  /cargo-home

The registry path keeps its `name-version/` structure, because that is what the parser reads a
package's identity from. No absolute path from any real machine, and no user name, remains; a test
in `tests/hygiene.rs` enforces that.
