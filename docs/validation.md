# Validation against real builds

The model in [model.md](model.md) is only worth anything if it reproduces builds that actually
happened. This document records what was measured, including where the model is wrong.

## Method

Four projects were chosen for different graph shapes: a small binary, a proc-macro-heavy library, a
deep workspace, and a wide one.

| project | commit | units | edges | shape |
| --- | --- | --- | --- | --- |
| ripgrep | `e89fff8` | 54 | 87 | small binary, shallow |
| async-graphql | `82cd1f1` | 119 | 198 | proc-macro heavy |
| rust-analyzer | `12c3381` | 308 | 870 | deep workspace |
| bevy | `c6f634c` | 419 | 1501 | wide workspace |

Each was built three times per configuration, `cargo clean` before every run, `--offline` so the
registry could not contribute network time, dev profile. Machine: Apple M1 Pro class, 10 cores,
macOS, `rustc 1.95.0`.

Predictions are compared against the **compilation span** — last unit end minus first unit start —
not against Cargo's reported total, because Cargo's total includes resolution and fetch time that is
outside the unit model. The reason that distinction matters is measured in
[cargo-data-sources.md](cargo-data-sources.md): a cold-registry build put 28 s of network time into a
32.5 s "build".

All figures are the median of three runs.

## Result 1: at the executor count the weights came from, the model is within 2%

| project | observed span | predicted | error |
| --- | --- | --- | --- |
| ripgrep | 5980 ms | 5990 ms | +0.2% |
| async-graphql | 18130 ms | 18360 ms | +1.3% |
| rust-analyzer | 60250 ms | 59210 ms | -1.7% |
| bevy | 165030 ms | 165100 ms | +0.0% |

Across 54 to 419 units and 6 s to 165 s of build time, the simulated makespan lands inside ±2% of
the real compilation span. This is the claim the tool rests on, and it is the only prediction it
makes that is this accurate.

It is worth being clear about what is and is not being tested here. The per-unit durations come from
the same build being predicted, so this measures whether the **schedule model** — pipelining, the
closure rule for units that link, the executor limit, Cargo's cost-blind priority — reproduces the
observed ordering. It does not test whether durations can be predicted; nothing here tries to.

## Result 2: these builds are limited by their graphs, not their cores

| project | total work | critical path | work / 10 | mean parallelism | limited by |
| --- | --- | --- | --- | --- | --- |
| ripgrep | 36160 ms | 5800 ms | 3616 ms | 6.05 | graph |
| async-graphql | 92710 ms | 18160 ms | 9271 ms | 5.11 | graph |
| rust-analyzer | 281400 ms | 58580 ms | 28140 ms | 4.67 | graph |
| bevy | 759210 ms | 164540 ms | 75921 ms | 4.60 | graph |

In every case the critical path is longer than total work divided by the executor count, by a factor
of 1.6 to 2.2. So more cores could not have helped these builds; the shape of the dependency graph is
what set the wall time.

Bevy is the extreme: its critical path is 164540 ms against an observed span of 165030 ms. **99.7% of
that build is one chain.** Adding cores to it would change essentially nothing.

Mean parallelism runs between 4.6 and 6.1 on a 10-core machine — roughly half the machine idle on
average. That is worth fixing, and it is what the tool is for.

A caution on a number that circulates widely: the claim that 32 cores yields an effective parallelism
of about three could not be sourced, and nothing here reproduces it. These are 10-core measurements
and they gave 4.6 to 6.1. No figure appears in this repository that was not measured here.

## Result 3: the critical path is short, so the advice is short

    ripgrep         6 units of  54 decide the build time
    async-graphql   8 units of 119
    rust-analyzer  22 units of 308
    bevy           see the tool's own output

ripgrep's, in full:

    regex-syntax    2080 ms  metadata at 1170
    regex-automata  2260 ms  metadata at 1550
    bstr             460 ms  metadata at  250
    globset          700 ms  metadata at  240
    ignore           930 ms  metadata at  480
    rg (bin)        1660 ms  requires its full closure

async-graphql's:

    syn                   2570 ms  metadata at 1430
    serde_derive          2390 ms  requires its full closure  (proc-macro)
    serde                 1280 ms  metadata at 1240
    bytes                  400 ms  metadata at  330
    async_graphql_value    580 ms  metadata at  320
    async_graphql_parser  1350 ms  metadata at  860
    async_graphql_derive  3090 ms  requires its full closure  (proc-macro)
    async_graphql         6870 ms  metadata at 4950

Two proc-macros sit on that path, and each one is a hard serialisation point: everything they
transitively depend on has to be completely built before they can start, because a proc-macro is a
dynamic library that has to be linked and loaded. This is the measured form of the folklore that
proc-macros serialise Rust builds, and it is why the closure rule in [model.md](model.md) section 3.1
is not an optional refinement.

## Result 4: the model is badly wrong when the executor count changes

This is the largest error in the tool and it is not fixable by improving the simulator.

Per-unit durations are not invariant to how many compilations run at once. Summing the durations
Cargo itself reports, at two different `-j` values on the same commit:

| project | total unit work at `-j 10` | at `-j 2` | change |
| --- | --- | --- | --- |
| ripgrep | 31540 ms | 24220 ms | -23.2% |
| async-graphql | 82710 ms | 57760 ms | -30.2% |

The same compilations are roughly a quarter to a third faster when fewer of them compete for memory
bandwidth and cache. So weights harvested at a high `-j` are inflated relative to a low-`-j` build.

Taking the graph and weights from a `-j 10` build and predicting a real `-j 2` build:

| project | observed `-j 2` | predicted | error |
| --- | --- | --- | --- |
| ripgrep | 13520 ms | 18130 ms | +34.1% |
| async-graphql | 33040 ms | 51290 ms | +55.2% |
| rust-analyzer | 116190 ms | 141310 ms | +21.6% |

Note that the area bound itself is violated: `ripgrep`'s work-over-two from `-j 10` weights is
18080 ms, and the real `-j 2` build finished in 13520 ms. A bound computed from weights measured
under different contention is not a bound.

The direction is consistent. **Extrapolating from a low executor count to a high one is optimistic;
from a high one to a low one is pessimistic.**

This is why the executor scan leads with the knee — the point beyond which the critical path
dominates — rather than with absolute times. The knee is decided by the critical path, which does not
depend on the executor count at all, so it survives a 30% error in the weights. An absolute makespan
does not.

## Result 5: the critical path is stable where it matters, and not at the margin

Three clean builds of the same commit at the same `-j`:

| project | identical path across 3 runs | critical path lengths | spread |
| --- | --- | --- | --- |
| ripgrep | yes | 5800 / 5970 / 5570 ms | 400 ms (6.9%) |
| async-graphql | no, 2 of 3 | 18160 / 16520 / 18030 ms | 1640 ms (9.1%) |
| rust-analyzer | no, 2 of 3 | 58580 / 58470 / 57260 ms | 1320 ms (2.3%) |

The disagreements are entirely in the **head** of the path and only ever between interchangeable
build-script chains. One async-graphql run began with `proc-macro2`'s build-script chain where the
others began with `quote`'s; rust-analyzer did the same. In every run the tail was identical — for
rust-analyzer, the same 17-unit chain from `syn` through to the `rust_analyzer` binary appeared all
three times.

So "which crate should I work on" is stable. "Which of two equivalent proc-macro build scripts starts
the chain" is not, and is not something anyone would act on.

This is why units are ranked by contribution rather than by membership of the path, and why slack is
reported against the measured spread.

## Result 6: the noise floor is real and some recommendations fall below it

The spread above is not a rounding detail. Splitting `regex-automata`, the heaviest unit on ripgrep's
critical path, was estimated to save **300 ms** — inside ripgrep's own 400 ms run-to-run spread. That
recommendation is indistinguishable from noise.

For contrast, splitting `async_graphql` was estimated to save 3395 ms against a 1640 ms spread, which
clears the floor by roughly a factor of two.

The spread is also not a fixed fraction of the build: 2.3% on the largest project against 9.1% on the
middle one. So no constant threshold would be right, and the tool measures the spread when it is given
several timing reports and declines to guess when it is not.

## What is not validated

- **Release builds.** Every number here is dev profile. In dev the compiler frontend is most of a
  unit's time, so pipelining has less to give and codegen is small. Release builds may well have a
  different critical path and a different parallelism, and nothing here establishes otherwise.
- **`cargo check`.** The graph for `cargo check` differs from a full build and is not measured here.
- **Platforms other than aarch64 macOS.** Nothing here was run on Linux or Windows.
- **Whether savings are realisable.** The tool predicts what a graph change would do to the schedule.
  It does not establish that the change is possible, or that a real refactor would divide a crate's
  compile time the way the model assumes.
- **Weights across a `Cargo.lock` change.** Every measurement holds the lockfile fixed.

## Reproducing this

The measurements come from clean, offline, full builds, three per configuration:

    cargo clean
    cargo build --timings -v --offline -j <N>

and the analysis reads the resulting `target/cargo-timings/cargo-timing.html` together with the
verbose build log. The commits are pinned in the table at the top. Absolute times will differ on other
hardware; the ratios — critical path against total work, and the direction of the executor-count error
— are what should reproduce.
