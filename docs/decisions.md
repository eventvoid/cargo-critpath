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

## D11 — The executor scan reports where the curve flattens, not absolute times

**Decided.** `--executors` reports the **knee**: the executor count beyond which the critical path
dominates the area bound, so more cores cannot help. Absolute makespans for executor counts far
from the one that was measured are still shown, but labelled unreliable, with the direction of the
error stated.

**Why.** Per-unit durations are not invariant to the executor count, and the effect is large.
Measured on this machine, comparing clean offline builds of the same commit at `-j 10` and `-j 2`,
taking the median of three runs each and summing the per-unit durations Cargo itself reports:

    project          total unit work at -j 10   at -j 2    change
    ripgrep                        31540 ms     24220 ms   -23.2%
    async-graphql                  82710 ms     57760 ms   -30.2%

The same compilations take roughly a quarter to a third less time when fewer of them run at once,
because they are not competing for memory bandwidth and cache. Weights harvested at a high `-j`
are therefore inflated relative to a low-`-j` build, and vice versa.

The consequence for prediction, measured the same way — graph and weights taken from a `-j 10`
build, then used to predict a real `-j 2` build:

    project          observed -j 2   predicted   error
    ripgrep               13520 ms    18130 ms   +34.1%
    async-graphql         33040 ms    51290 ms   +55.2%

At the executor count the weights came from, the same simulator was within +0.2% and +1.3%. So the
simulator is not the problem; reusing weights across a change in contention is.

The direction is consistent and worth stating plainly: **extrapolating from a low executor count to
a high one is optimistic, and from a high one to a low one is pessimistic.**

The knee, by contrast, is robust. It is decided by the critical path, which does not depend on the
executor count at all, against total work, which rescales but does not reorder. "Beyond about six
cores this build stops getting faster" survives a 30% error in the weights; "this build takes
18.1 s on two cores" does not.

**Rejected: dropping the scenario.** The knee is the most actionable single number the tool
produces, and it is trustworthy.

**Rejected: reporting absolute times without the caveat.** That is the failure mode this project
exists to avoid. A number with 34% to 55% of error in it, printed in the same typeface as a number
with 1% of error in it, is a lie of presentation.

**Rejected: calibrating the contention effect out.** Two measurements would give a correction
factor for this machine and this project, and it would not transfer to another machine, another
core count, or another `codegen-units`. Fitting a curve through two points and presenting the
result as a prediction would be worse than declining to predict.

## D12 — A split is reported as a sweep over the division fraction

**Decided.** The split scenario sweeps the fraction `f` that divides the crate's work, and reports
the range of savings across that sweep alongside the dependent-assignment bounds.

**Why.** The optimistic and pessimistic dependent assumptions were expected to bracket the
uncertainty. Measured against real critical paths, they do not. At the default `f = 1/2` both halves
finish simultaneously, so the assignment of dependents cannot affect any downstream start time, and
the two bounds coincide exactly:

    ripgrep        split regex-automata (2260ms)  saving 300ms  under both assumptions
    async-graphql  split async_graphql  (6870ms)  saving 3395ms under both assumptions

An interval whose two ends are equal is not an interval, and printing one implies a robustness the
number does not have. To be precise about which number coincides: the two assumptions give the same
**critical path**. They need not give the same simulated makespan, because the pessimistic graph has
twice the outgoing edges and the `cargo-like` priority counts transitive reverse-dependents, so the
two halves are ordered differently. The variable that actually drives the answer is `f`, and `f` is precisely
what cannot be known before performing the split.

**Rejected: keeping only the optimistic/pessimistic pair.** It is honest about a real unknown that
happens not to bind at the default, while staying silent about the unknown that does bind.

**Rejected: a single number at `f = 1/2`.** Splitting a crate exactly in half by compile time is
the least likely outcome of a real refactor.

## D13 — A saving is not reported as a saving unless it clears the noise floor

**Decided.** The tool accepts more than one timing report for the same build
(`--timings <path>` repeated). With several it measures the run-to-run spread and marks any saving
below that spread as not actionable. With only one it cannot know the spread, says so, and cites the
measured figures below rather than inventing a threshold.

**Why.** Three clean builds per project at `-j 10` gave these critical path lengths:

    ripgrep          5800 /  5970 /  5570 ms    spread   400 ms   ( 6.9% of the median)
    async-graphql   19800 / 18000 / 18030 ms    spread  1800 ms   (10.0%)
    rust-analyzer   60150 / 59270 / 58840 ms    spread  1310 ms   ( 2.2%)
    bevy           164540 / 147450 / 151510 ms  spread 17090 ms   (11.3%)

Bevy's spread alone is 17 seconds. A saving smaller than that, printed on its own, is
indistinguishable from noise while looking like a finding, and a tool that recommends work on the
strength of a number smaller than its own measurement error is worse than no tool — the first
non-negotiable in this project.

Note also that the spread is not a fixed percentage: 2.2% on the deepest project against 11.3% on
the widest. So a hard-coded percentage threshold would be wrong in both directions, which is why the
spread is measured when it can be and declared unknown when it cannot.

**Rejected: a fixed percentage threshold.** The three measurements above span 2.3% to 9.1%. Any
constant is wrong somewhere, and being wrong quietly is the failure mode that matters.

**Rejected: reporting savings unqualified and leaving the judgement to the reader.** The reader has
no way to know the spread — it takes several clean builds to find out, which is exactly the work the
tool is supposed to have done for them.

**Rejected: suppressing sub-threshold savings entirely.** They are still evidence about the shape of
the graph, and hiding data is its own dishonesty. They are labelled, not removed, and they sort below
everything that clears the floor.

## D14 — Rank units by contribution, not by membership of the path

**Decided.** The primary output ranks units by how much time they contribute to the critical path.
Membership of the path is shown but is not the ranking key.

**Why.** Across three repeat builds of the same commit, the *tail* of the critical path was
identical every time, but the *head* was not: async-graphql sometimes began with `proc-macro2`'s
build-script chain and sometimes with `quote`'s, and rust-analyzer did the same, because those chains
are interchangeable and near-equal in length. Path membership at the margin therefore flips between
runs of the same build, while the ranking by contribution does not move.

Reporting "these 22 units are on the critical path" invites a reader to trust a boundary that is not
stable. Reporting "these units contribute this much" is stable, and is the thing worth acting on.

## D15 — Only `cargo build` is supported; other subcommands are refused by name

**Decided.** The tool analyses `cargo build`, in any profile. A timing report from `cargo check`,
`cargo test`, `cargo bench` or `cargo doc` is refused with a message naming the subcommand it
detected and saying it is not supported, rather than analysed.

**Why.** These are genuinely different builds, not variations on one. Measured on ripgrep, the dev
build and the `cargo check` build share **exactly one unit** on their critical paths — the final
binary. Check's critical path runs through a third-party build-script execution; dev's runs through
the regex crates. Pipelining is worth 28% of the critical path in the dev build and 1% in check,
because with `--emit=metadata` there is nothing after the metadata point to pipeline past.

So advice derived from one is not advice about the other, and a tool that silently accepted either
would be answering a question the user did not ask. `cargo check` is also the command developers run
most, which makes the confusion more likely rather than less.

Refusing by name is not the same as failing to parse. A parser written against the `build` alphabet
alone fails to match 33 of ripgrep's 54 check units and yields a 21-node graph with a nonsense
critical path — a wrong answer rather than a refusal. The subcommand is detected from the target
strings recorded in [cargo-data-sources.md](cargo-data-sources.md) and reported explicitly.

**Rejected: supporting `cargo check`.** It is cheap to collect, it is what developers run in a loop,
and the model is *simpler* there because it degenerates to a plain longest path. It is the obvious
next feature. It is not in this release because it has not been validated, and shipping an unvalidated
second mode alongside a validated one would undo the point of validating the first.

**Rejected: analysing whatever it is given.** That is the behaviour that produces a confident wrong
answer, which the first non-negotiable of this project forbids.

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
