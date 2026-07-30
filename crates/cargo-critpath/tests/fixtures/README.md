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
