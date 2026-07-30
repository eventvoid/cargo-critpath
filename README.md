# cargo-critpath

Our workspace builds on a 10-core machine and averages about five cores busy. Adding cores does
nothing. `cargo build --timings` shows the gaps clearly enough, but it does not say which crate to
change, and it does not say what changing it would buy.

This tool answers that. It reads a build's dependency graph and its per-unit compile times, simulates
the schedule, finds the critical path, and estimates what a specific change to the graph would save.

The most useful thing it tells you is negative. Sorting crates by how long they take — which
`cargo build --timings` already gives you for free — cannot distinguish a crate that holds the build
back from one that compiles in parallel with something longer and could be made instant for no gain.
On ripgrep, five of the ten longest-compiling units contribute nothing to the critical path at all.

Measured on four real projects from 54 to 419 units, all four were limited by the shape of their
dependency graph rather than by available cores. On bevy the critical path is 99.5% of the entire
build. Every number in this repository was measured, and the measurements are published together with
the cases where the model is wrong: see [docs/validation.md](docs/validation.md).

## What it looks like

Three clean builds of ripgrep at `-j 10`, passing all three timing reports so the tool can measure
the run-to-run spread. Abridged only by cutting two of the five split scenarios.

```
build
  54 units and 36160ms of work, over 89 edges, 60 of them satisfied by metadata
  dev profile, rustc 1.95.0 (59807616e 2026-04-14), target aarch64-apple-darwin
  analysed for 10 executors; the build ran with -j 10 and reached a peak of 12 units at once
  6100ms reported in total, 120ms of it outside the unit model
    Fetching, resolution and anything else before the first unit started. Reported here rather than
    folded into the numbers below, which are about compilation only.

bounds
  critical path          5800ms   holds under every scheduling policy
  total work / 10        3616ms
  lower bound            5800ms   constraint: graph, the shape of the build is what holds it back
  simulated makespan     6040ms   an estimate under the cargo-like policy, not a bound
  mean parallelism         5.99   total work over the simulated makespan

units by contribution to the critical path
  What each unit adds to the 5800ms. Ranked by contribution rather than by membership of the path,
  because the head of the path flips between runs of the same build and the contributions do not.
  They sum to the critical path exactly.
     1660ms  ripgrep 15.2.0 (rg)  [bin]
     1550ms  regex-automata 0.4.15 (lib)  [lib]
     1170ms  regex-syntax 0.8.11 (lib)  [lib]
      930ms  ignore 0.4.29 (lib)  [lib]
      250ms  bstr 1.13.0 (lib)  [lib]
      240ms  globset 0.4.19 (lib)  [lib]

the critical path, in order
      enter       exit       adds  unit
        0ms     2080ms     1170ms  regex-syntax 0.8.11 (lib)
     1170ms     3430ms     1550ms  regex-automata 0.4.15 (lib)  (entered on the metadata of the unit above)
     2720ms     3180ms      250ms  bstr 1.13.0 (lib)  (entered on the metadata of the unit above)
     2970ms     3670ms      240ms  globset 0.4.19 (lib)  (entered on the metadata of the unit above)
     3210ms     4140ms      930ms  ignore 0.4.29 (lib)  (entered on the metadata of the unit above)
     4140ms     5800ms     1660ms  ripgrep 15.2.0 (rg)

what splitting a crate could do
  A split cannot be modelled exactly, because which half each dependent would need is not knowable
  without performing it. Each figure below is a range over a sweep of the division fraction from
  0.25 to 0.75 under both dependent assumptions. At an even division those two assumptions give the
  same critical path, so what the range is really made of is the sweep over the division. Each half
  is charged 0ms of per-crate overhead, because nobody has measured what a real split costs and
  inventing a number would be worse than charging nothing and saying so.

  ripgrep 15.2.0 (rg)
    critical path     4970ms to 5385ms     against 5800ms as it stands
    saving             415ms to 830ms
    makespan          5210ms to 5625ms     estimate, not a bound

  regex-automata 0.4.15 (lib)
    critical path     5500ms to 5500ms     against 5800ms as it stands
    saving             300ms to 300ms
    makespan          5830ms to 6020ms     estimate, not a bound
    every division in the sweep gives the same critical path, because what the split releases is
    above rather than removed.

executors
  The knee is at 7 executors: from there on the critical path dominates.

     J   work / J  lower bound   simulated   constraint  reliability
     1    36160ms      36160ms     36160ms   machine     pessimistic
     2    18080ms      18080ms     19260ms   machine     pessimistic
     4     9040ms       9040ms     11120ms   machine     pessimistic
     7     5166ms       5800ms      7490ms   graph       knee, pessimistic
     8     4520ms       5800ms      6710ms   graph       pessimistic
    10     3616ms       5800ms      6040ms   graph       weights measured here
    16     2260ms       5800ms      5800ms   graph       optimistic

notes
  warning [saving-below-noise-floor] estimated saving of at most 300ms for regex-automata 0.4.15
  (lib) is smaller than the 400ms run-to-run spread measured across the 3 supplied timing reports
```

Note what the last line does. The second-largest unit on the critical path is also the one whose
estimated saving is inside this project's own measurement noise, and the tool says so rather than
letting the number stand.

## What it is not

It is not a profiler. `cargo build --timings` already tells you where the time went, in one flag with
nothing to install.

**It says nothing about the incremental rebuild you actually wait on.** This analyses a clean build.
Ranking a workspace's crates by clean-build critical path against ranking them by what a touch costs
to rebuild shared no member of the top five on either of the two largest projects measured. A reader
who confuses the two will work on the wrong crate. Reporting rebuild cost as well is the most useful
thing this tool does not yet do.

It also says when it has nothing to offer. If total work divided by your core count exceeds the
critical path, the machine is the constraint and restructuring will not help. At `-j 2` all four
measured projects fall into exactly that case, which is why the constraint column above says
`machine` for the first three rows.

## Installation

    cargo install cargo-critpath

Rust 1.74 or newer, no system dependencies. The tool depends on two crates, `serde_json` and
`lexopt`, and pulls no procedural macros — [docs/decisions.md](docs/decisions.md) D3 explains why that
was worth hand-writing the argument parsing for.

## Use

It needs a clean, full build. A cached unit is reported with a duration of zero, so a graph built from
an incremental build is almost entirely zero-weight, and the tool refuses it. `-v` gives the
structure, `--timings` the weights, and `cargo metadata` attributes workspace-local crates to
packages — that last one is not optional, since guessing from source paths silently dropped ten of
bevy's units in an earlier attempt.

    cargo clean
    cargo build --timings -v --offline 2> build.log
    cargo metadata --format-version 1 --no-deps --offline > metadata.json
    cargo critpath --log build.log \
                   --timings target/cargo-timings/cargo-timing.html \
                   --metadata metadata.json

`--offline` is worth using: on a cold registry we measured 28 s of network time inside a 32.5 s
"build". Any such prefix is reported separately rather than folded in.

Pass `--timings` once per repeated build and the tool measures your run-to-run spread and marks
savings that fall inside it. With one report it says the noise floor is unknown rather than inventing
a threshold.

`--format json` writes the versioned schema in [docs/json-schema.md](docs/json-schema.md). Exit code
3 means a `--fail-if` threshold was exceeded, distinct from 1 for a usage error and 2 for unusable
input, so a CI script never has to parse prose.

Only `cargo build` is supported, in any profile. A report from `cargo check`, `cargo test` or
`cargo bench` is refused by name: they are genuinely different builds, and on ripgrep the dev and
check critical paths share exactly one unit.

## How far to trust the numbers

The model is in [docs/model.md](docs/model.md). What matters before reading an estimate:

**Pipelining is accounted for.** A dependent starts when its dependency emits metadata, not after full
codegen. Ignoring it overstates the critical path by 3% to 33% on the measured projects.

**Accuracy, measured.** Given a build's own weights, the simulated makespan landed within **2.1% of
the real compilation span on all 24 captured builds**, and within 1% on 18 of them.

**The graph is checked against the build's own record of itself.** Cargo's timing report notes which
units each completion unblocked. Those notes are incomplete, but every pair in them is a real edge,
and all 35, 74, 203 and 299 of them appear in the parsed graphs of the four projects.

**The headline number is the critical path, not a simulated wall time.** The critical path and the
bound beside it hold under any scheduling policy. The simulated makespan depends on one, and Cargo's
own is cost-blind and breaks ties by hash order, so it is not reproducible even in principle.

## Where the estimates are unreliable

This section is the reason to trust the rest.

**Changing the job count is the weakest thing it does.** Per-unit compile times are not invariant to
how many compilations run at once: the same units took 23% less total time at `-j 2` than at `-j 10`
on ripgrep and 30% less on async-graphql. Predicting a `-j 2` build from `-j 10` timings was 34% and
55% too slow, and the area bound is not even a bound across such a change. Hence the knee, and the
reliability column.

**The knee itself moves by one.** Three repeated clean builds of ripgrep put it at 7, 6 and 6, because
total work varies run to run. Read it as a small range.

**More executors can make the simulated build longer.** A real property of greedy scheduling, not a
bug, reproduced independently at 95 violations in 19800 comparisons. The simulated column is not
monotone and must not be read as one.

**Splitting a crate cannot be modelled exactly.** Which half each dependent needs is unknowable
without doing the split. At an even division the optimistic and pessimistic dependent assumptions
give the same answer, so that pair is not where the uncertainty lives; the sweep over the division is.

**Savings can fall below the noise floor.** Critical-path spread across three identical builds was
400 ms on ripgrep and 17 s on bevy — 2.2% to 11.3%, not a fixed fraction.

**Most of a critical path may not be yours to change.** Workspace-owned share was 49% on ripgrep, 64%
on async-graphql, 90% on rust-analyzer and 92% on bevy. On a small binary crate most of the advice
concerns crates you do not own, where the only real options are dropping a dependency, disabling a
feature, or upstreaming a change.

**Only dev-profile builds on aarch64 macOS are validated.**

## The data it reads

Both inputs are documented, with the surprises, in
[docs/cargo-data-sources.md](docs/cargo-data-sources.md). Three findings shaped the design:

- There is **no JSON timing output**. `--timings` is a valueless flag on stable and nightly alike; the
  report is HTML and the parser extracts the JavaScript data islands it embeds. That is an
  undocumented internal format, so the parser reports what it found and fails with a specific message
  rather than a wrong number when the shape changes.
- The report's `unblocked_units` fields are **not** the dependency graph — they cover about half the
  edges. They are used as a check instead.
- The report's `mode` field is the literal string `"todo"` for almost every unit, and every library
  target renders identically whether it is an rlib or a proc-macro, which is the distinction that
  decides half the edge kinds. The structure therefore comes from the verbose log.

## Layout

    crates/critpath-core     the schedule model: graph, simulation, metrics, transformations.
                             No dependencies, no I/O, no knowledge of Cargo. Supporting another
                             build system means writing another adapter.
    crates/cargo-critpath    the Cargo adapters, the CLI, and the output formats.

    docs/model.md            the schedule model, its invariants, and its systematic errors
    docs/validation.md       measurements against four real projects, including the failures
    docs/cargo-data-sources.md   what Cargo emits, established by running it
    docs/decisions.md        every design decision, what was rejected, and why
    docs/json-schema.md      the machine-readable output contract

## Licence

MIT or Apache-2.0, at your option.
