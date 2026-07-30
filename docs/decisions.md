# Decision log

Each entry records what was decided, what was rejected, and why. Where a decision rests on a
number, the number was measured on the machine and command named in the entry.

---

## D1 — Two crates, with the core unable to see Cargo

**Decided.** A workspace of `critpath-core` (graph, simulation, metrics, transformations) and
`cargo-critpath` (Cargo adapters, rendering, CLI). `critpath-core` has no dependencies at all and
performs no I/O.

**Rejected: one crate with a module boundary.** A module boundary is a convention; a crate
boundary is checked by the compiler. Since "usable for another build system by swapping the
adapter" is a requirement rather than an aspiration, it should not be possible to reach Cargo
types from the core by accident.

**Rejected: three crates**, splitting the Cargo adapter from the CLI. The adapter is only useful
to something that also renders results, and a third manifest buys nothing today. Revisit if a
second front end appears.

**Cost.** Two manifests, and public types in the core that would otherwise have stayed private.
That second cost is real and is the reason the core's public surface is deliberately narrow.

---

## D2 — Integer milliseconds, not floating-point seconds

**Decided.** Durations are `Millis(u64)`.

Cargo reports two decimal places of seconds, so 10 ms is the input resolution and milliseconds
lose nothing. Integers make ordering total, sums exact and output reproducible, which invariant I1
in [model.md](model.md) requires.

**Rejected: `f64` seconds.** Sorting `f64` needs a total order that does not exist for `NaN`,
sums are not associative, and two runs on different platforms can print different digits.
Determinism is a stated requirement, so the arithmetic should not be the thing that threatens it.

**Rejected: `std::time::Duration`.** Nanosecond precision the input does not have, no cheap
saturating arithmetic, and it invites treating a weight as a wall-clock measurement rather than a
graph weight. The core is not measuring anything; it is doing arithmetic on given numbers.

---

## D3 — Dependencies: `serde_json` and `lexopt`, nothing else

**Decided.** The whole tool depends on `serde_json` and `lexopt`. `critpath-core` depends on
nothing.

Measured on this machine with `cargo build --offline` after `cargo clean`, 10 cores:

    dependency tree:  lexopt 0.3.2, serde_json 1.0.151, itoa, memchr, serde_core, zmij
    6 crates, 13 compilation units, 0 proc-macro units, 6 build-script units
    clean debug build: 3.25s

**Rejected: `clap`.** Better help text and better argument errors than anything hand-written, and
it is the conventional choice. It also pulls a derive macro, and a proc-macro unit is a full
serialisation point in the very graph this tool exists to complain about. The measured contrast
is stark: adding `serde` with `derive`, `serde_json` and `thiserror` to an empty binary produced
28 units with 2 proc-macros and about 5.1 s of compilation, against 13 units and 3.25 s here. A
tool whose thesis is that graph shape dominates build time should not ship a shape it would
criticise. The cost of this decision is hand-written argument parsing and help text, which is
more code and a plausible source of bugs; it is covered by CLI tests.

**Rejected: `serde` with `derive`.** Same reason. Cargo's JSON is read by navigating
`serde_json::Value`, and the tool's own JSON output is constructed explicitly. That is more
verbose than `#[derive(Deserialize)]` and it is the reason there are no proc-macros in the tree.

**Rejected: `thiserror` / `anyhow`.** Error types are written out by hand with their `Display` and
`Error` impls. `thiserror` is a proc-macro; `anyhow` erases the typed error set that the quality
bar requires the library to expose.

**Rejected: an HTML parser for the timing report.** The report embeds JavaScript array literals,
which are JSON. The parser locates the assignment and hands the literal to `serde_json`. A DOM
parser would not make an undocumented format less fragile.

**Rejected: `regex`.** The verbose `rustc` lines are scanned with ordinary string operations. The
patterns are fixed and simple, and `regex` is larger than the rest of the tree combined.

**Licences.** `lexopt` is MIT. `serde_json`, `itoa`, `memchr`, `serde_core` and `zmij` are
`MIT OR Apache-2.0`. All compatible with this project's `MIT OR Apache-2.0`.

---

## D4 — Version 0.1.0, with the JSON schema versioned separately

**Decided.** Release `0.1.0`. The machine-readable output carries its own `schema_version`,
starting at 1.

The two things need different promises. The CLI surface and the JSON schema are contracts that
consumers build scripts against, and breaking them silently is not acceptable — hence an explicit
integer schema version, bumped only on a breaking change, with a test that pins the shape. The
Rust library API has not yet been used by anyone outside this repository, and an API review is
expected to change it; claiming `1.0.0` would promise stability for a surface that has not earned
it.

**Rejected: `1.0.0`.** It would assert the library API is settled. It is not.

**Rejected: starting at a higher `0.x` to look established.** Cosmetic, and the log would have to
lie about why.

---

## D5 — Structure from `cargo build -v`, weights from `--timings`

**Decided.** One `cargo build --timings -v --offline` produces both inputs. Structure and edge
kinds are parsed from the verbose `rustc` invocations; weights and the metadata split come from
the timing report.

The reasoning is in [cargo-data-sources.md](cargo-data-sources.md). In short: the timing report's
`unblocked_units` fields covered only 51% of the real edges on a 28-unit build, so the graph
cannot be recovered from timings alone; and `--unit-graph`, which would give it exactly, is
nightly-only.

**Rejected: requiring nightly for `--unit-graph`.** It is the cleanest structural input and it is
supported as an option, but requiring a nightly toolchain to analyse a stable build is a steep
price for data that the verbose output already contains exactly.

**Rejected: reconstructing the unit graph from `cargo metadata` alone.** `cargo metadata` gives
the *package* graph. Deriving units from it means reimplementing Cargo's unit-construction rules
— build-script triple-units, host versus target, per-unit feature resolution — and being wrong in
ways that are invisible. The verbose output is Cargo stating what it actually did.

**Rejected: a `RUSTC_WRAPPER` that times invocations itself.** It would give exact per-unit wall
times on stable without parsing HTML, and it was tempting. It cannot observe the
metadata-emission moment inside a `rustc` run, which is precisely the quantity that keeps the
critical path from being overstated. Worth revisiting if Cargo ever exposes sections on stable
for all units.

---

## D6 — A clean, offline, full build is required, and enforced

**Decided.** The tool rejects input from a build with a non-trivial fraction of fresh units, and
recommends `--offline`.

Both come from measurements in [cargo-data-sources.md](cargo-data-sources.md): fresh units appear
in the timing report with `duration` 0.0, so an incremental build yields a graph that is almost
entirely zero-weight; and a cold registry put 28 s of network time into a 32.5 s "build".

Reporting a critical path computed from mostly-zero weights would be the single most misleading
thing this tool could do, so it is an input error and not a warning.

---

## D7 — Lead with the critical path, report the simulation as an estimate

**Decided.** The headline number is the critical path length `CP`, together with the bound
`LB(J) = max(CP, total_work / J)`. The simulated `makespan(J)` is reported next to them, labelled
as an estimate and with its scheduling policy named.

`CP` and `LB(J)` hold for every scheduling policy; `makespan(J)` depends on a policy that is not
Cargo's. Leading with the guaranteed quantities means the primary output does not move when the
simulator's tie-breaking changes.

This also gives the tool an honest way to say it is not needed: when `total_work / J` dominates
`CP`, the machine is the constraint, restructuring the graph will not help, and the output says so
instead of ranking recommendations nobody should act on.

---

## D8 — Counterfactuals report intervals, and the anomaly is disclosed

**Decided.** A split reports an optimistic and a pessimistic bound with the assumptions printed,
and no point estimate. Edge removal is anchored on the change in `CP`.

Which half of a split crate each dependent needs is not knowable without doing the split, so any
single number would be a guess wearing a confident tone.

Anchoring edge removal on `CP` is not stylistic. Removing a precedence constraint can *increase*
the makespan of a greedy list schedule — a Graham anomaly, and a real property of the scheduler
rather than a bug. `CP` is monotone under edge removal and `makespan` is not, so `CP` carries the
claim and a wrong-direction move in the simulation is disclosed rather than clamped to zero.

---

## D9 — MSRV 1.74.0

**Decided.** `rust-version = "1.74.0"`, checked in CI on that exact toolchain.

`serde_json` declares 1.71 and `lexopt` declares none, so 1.71 is the floor imposed by
dependencies. 1.74 sits just above it and is old enough to be present in the distributions and
CI images that lag. Nothing in the code needs anything newer; the CI job is what keeps that true
rather than a claim in a manifest.

---

## D10 — Lints denied at the workspace level

**Decided.** `unsafe_code` forbidden; `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`,
`todo`, `unimplemented`, `dbg_macro`, `print_stdout` and `print_stderr` denied; `missing_docs`
warned. CI runs `clippy --all-targets --all-features -D warnings`.

Denying `indexing_slicing` is the uncomfortable one: it forces `.get()` and explicit handling
throughout the graph traversals, which is more code than `graph[i]`. It is also the lint that
makes "no panics in library code" true rather than aspirational, since slice indexing is the way
a panic normally arrives.

`print_stdout` and `print_stderr` are denied so that writing to a stream is confined to the
rendering layer, where the destination is a parameter and can be captured by a test.

These are relaxed per-module where the alternative is worse, with a comment on each exception
stating why.
