# Validation against real builds

The model in [model.md](model.md) is only worth anything if it reproduces builds that actually
happened. This document records what was measured, including where the model is wrong.

## Provenance of these figures

**These numbers were produced by a reference implementation of the model, not yet by the shipped
tool.** The model is defined in [model.md](model.md); the reference implementation follows it
independently so that agreement between the two is evidence rather than a tautology. The captured
builds are real, the timings are Cargo's own, and every figure below is reproducible from the
commands in the last section — but reproducing them from this repository requires the analysis and
adapter code, which is why this notice is here and will be removed when the tool regenerates these
tables itself.

## Method

Four projects were chosen for different graph shapes.

| project | commit | units | edges | shape |
| --- | --- | --- | --- | --- |
| ripgrep | `e89fff8` | 54 | 89 | small binary, shallow |
| async-graphql | `82cd1f1` | 119 | 198 | proc-macro heavy |
| rust-analyzer | `12c3381` | 308 | 870 | deep workspace |
| bevy | `c6f634c` | 419 | 1501 | wide workspace |

Each was built three times per configuration, `cargo clean` before every run, `--offline`, dev
profile, at `-j 10` and at `-j 2`. Machine: Apple M1 Pro class, 10 cores, macOS, `rustc 1.95.0`.

Predictions are compared against the **compilation span** — last unit end minus first unit start —
not Cargo's reported total, which includes resolution and fetch time outside the unit model. On a
cold registry that difference was 28 s of a 32.5 s "build".

Unless a row says otherwise, a figure is the **median of three runs**, and per-run values are given
wherever the spread matters.

## Result 1: the model reproduces a build it has the weights for

For each run, the graph and weights are taken from **that run** and used to predict **that run's**
span. This isolates the schedule model — pipelining, the closure rule, the executor limit, the
priority rule — from run-to-run variation in how long units take, which is a separate question
measured in result 4.

| project | `-j 10` median error | worst | `-j 2` median error | worst |
| --- | --- | --- | --- | --- |
| ripgrep | -0.2% | -2.8% | -0.1% | -0.3% |
| async-graphql | -7.4% | -8.4% | -0.0% | +0.1% |
| rust-analyzer | -2.4% | -2.8% | -3.2% | -3.2% |
| bevy | -0.2% | -0.6% | +0.1% | -0.2% |

Three of the four land within 3% at both job counts. **async-graphql at `-j 10` does not**: two of
its three runs came in 7.4% and 8.4% below the observed span, while the third was exact. The
critical path accounted for 99.4% of the span in the run that matched and only 91.6% in the two that
did not, so roughly 8% of those builds is time the model does not account for. That is an open gap,
not a rounding error, and it is the largest unexplained discrepancy in this table.

Note that the error is negative almost everywhere: the model predicts a slightly *faster* build than
happened. That is the expected direction, since the simulation charges nothing for process startup,
for a `rustc` holding more than one jobserver token, or for any scheduling delay Cargo incurs
between a unit becoming ready and actually starting.

## Result 2: at ten cores these builds are limited by their graphs

Medians of three runs at `-j 10`:

| project | total work | critical path | work / 10 | span | mean parallelism | CP / span |
| --- | --- | --- | --- | --- | --- | --- |
| ripgrep | 31540 ms | 5800 ms | 3154 ms | 5980 ms | 5.27 | 0.970 |
| async-graphql | 82710 ms | 18030 ms | 8271 ms | 18130 ms | 4.56 | 0.994 |
| rust-analyzer | 269150 ms | 58470 ms | 26915 ms | 60250 ms | 4.47 | 0.970 |
| bevy | 654880 ms | 151510 ms | 65488 ms | 152250 ms | 4.30 | 0.995 |

In every case the critical path exceeds total work divided by the core count, by a factor of 1.8 to
2.3. More cores could not have helped these builds. The critical path accounts for 97% to 99.5% of
the observed span — these builds very nearly *are* their critical paths.

Mean parallelism runs between 4.3 and 5.3 on a 10-core machine: roughly half the machine idle on
average, and the larger the project, the worse it gets.

## Result 3: at two cores the same builds are not graph-limited

The same projects, same commits, at `-j 2`:

| project | CP / span at `-j 10` | CP / span at `-j 2` |
| --- | --- | --- |
| ripgrep | 0.970 | 0.493 |
| async-graphql | 0.994 | 0.538 |
| rust-analyzer | 0.970 | 0.460 |
| bevy | 0.995 | 0.501 |

At two cores the critical path is about half the span, so the machine is the constraint and
restructuring the graph would buy almost nothing. This is the distinction the tool reports as
`constraint: graph` versus `constraint: machine`, and it is why it leads with the comparison rather
than with a recommendation.

It also means the thesis is a statement about *machines with many cores*. On a small machine these
graphs are fine.

## Result 4: weights do not survive a change in the job count

This is the largest error in the tool and improving the simulator cannot fix it.

Per-unit durations are not invariant to how many compilations run at once. Summing the durations
Cargo reports, on the same commit:

| project | total unit work at `-j 10` | at `-j 2` | change |
| --- | --- | --- | --- |
| ripgrep | 31540 ms | 24220 ms | -23.2% |
| async-graphql | 82710 ms | 57760 ms | -30.2% |

The same compilations are a quarter to a third faster when fewer of them compete for memory
bandwidth. Result 1 shows the model is accurate at `-j 2` when given `-j 2` weights; it is carrying
weights across the change that fails.

Taking the graph and weights from a `-j 10` build and predicting a real `-j 2` build:

| project | observed `-j 2` | predicted | error |
| --- | --- | --- | --- |
| ripgrep | 13520 ms | 18130 ms | +34.1% |
| async-graphql | 33040 ms | 51290 ms | +55.2% |
| rust-analyzer | 116190 ms | 141310 ms | +21.6% |
| bevy | 305130 ms | 380040 ms | +24.6% |

The area bound is itself violated: ripgrep's work-over-two computed from `-j 10` weights is 18080 ms
and the real `-j 2` build finished in 13520 ms. A bound computed from weights measured under
different contention is not a bound.

The direction is consistent. **Extrapolating from a low job count to a high one is optimistic; from a
high one to a low one is pessimistic.** This is why the executor scan leads with the knee, which is
decided by the critical path and so does not depend on the job count, rather than with absolute
times.

## Result 5: the critical path is stable where it matters

Three clean builds of the same commit at `-j 10`:

| project | identical path across 3 runs | critical path lengths | spread |
| --- | --- | --- | --- |
| ripgrep | yes | 5800 / 5970 / 5570 ms | 400 ms (6.9%) |
| async-graphql | no, 2 of 3 | 18160 / 16520 / 18030 ms | 1640 ms (9.1%) |
| rust-analyzer | no, 2 of 3 | 58580 / 58470 / 57260 ms | 1320 ms (2.3%) |
| bevy | yes | 164540 / 146650 / 151510 ms | 17890 ms (11.8%) |

The disagreements are entirely in the **head** of the path, and only between interchangeable
build-script chains. One async-graphql run began with `proc-macro2`'s build-script chain where the
others began with `quote`'s; rust-analyzer did the same. In every run the tail was identical — for
rust-analyzer, the same 17-unit chain from `syn` through to the `rust_analyzer` binary appeared all
three times.

So "which crate should I work on" is stable. "Which of two equivalent build scripts starts the chain"
is not, and is not something anyone would act on. This is why units are ranked by contribution rather
than by membership of the path.

Bevy's 11.8% spread is a caution about the whole exercise: its first run was 165030 ms and its
second 147960 ms, a difference far larger than any saving the tool would report for a single change.
First runs after a `cargo clean` appear to be systematically slower, so a single measurement of a
large project should not be trusted.

## Result 6: which parts of the model earn their place

The model has two refinements over a naive longest-path computation. They are not worth the same.

Recomputing the critical path with every metadata edge treated as a full edge — that is, ignoring
pipelining entirely:

| project | critical path | ignoring pipelining | overstatement |
| --- | --- | --- | --- |
| ripgrep | 5800 ms | 8090 ms | 28.3% |
| async-graphql | 18160 ms | 18530 ms | 2.0% |
| rust-analyzer | 58580 ms | 85620 ms | 31.6% |
| bevy | 164540 ms | 215500 ms | 23.6% |

Recomputing it with the closure requirement of [model.md](model.md) section 3.1 cleared on every
unit:

| project | critical path | ignoring the closure rule | difference |
| --- | --- | --- | --- |
| ripgrep | 5800 ms | 5800 ms | 0 ms |
| async-graphql | 18160 ms | 18160 ms | 0 ms |
| rust-analyzer | 58580 ms | 58580 ms | 0 ms |
| bevy | 164540 ms | 164540 ms | 0 ms |

So pipelining is load-bearing and the closure rule, on these four projects, is not. The 30% figure
quoted for the closure rule elsewhere comes from a fixture constructed to exhibit it, and that
distinction is now made wherever the rule is described. The rule stays because it is what Cargo
does and it can bind on graphs these four do not cover, but nobody should believe it is what makes
the tool accurate.

## Result 7: some recommendations fall below the noise floor

Splitting `regex-automata`, the heaviest unit on ripgrep's critical path, was estimated to save
**300 ms** — inside ripgrep's own 400 ms run-to-run spread. That recommendation is indistinguishable
from noise and must be labelled so.

For contrast, splitting `async_graphql` was estimated to save 3395 ms against a 1640 ms spread,
which clears the floor by roughly a factor of two.

The spread is not a fixed fraction of the build — 2.3% on rust-analyzer against 11.8% on bevy — so
no constant threshold would be right. The tool measures the spread when given several timing reports
and declines to guess when given one.

## What is not validated

- **Release builds.** Every number here is dev profile, where the compiler frontend dominates each
  unit and pipelining has less to give.
- **`cargo check`**, which is what most developers run in a loop, has a different graph.
- **Platforms other than aarch64 macOS.**
- **The 8% gap on async-graphql at `-j 10`** is unexplained.
- **Whether savings are realisable.** The tool predicts what a graph change would do to a schedule.
  It does not establish that the change is possible, nor that a real refactor would divide a crate's
  compile time the way the model assumes.
- **Anything across a `Cargo.lock` change.** Every measurement holds the lockfile fixed.

## Reproducing this

    cargo clean
    cargo build --timings -v --offline -j <N>

reading the resulting `target/cargo-timings/cargo-timing.html` together with the verbose build log,
and `cargo metadata --format-version 1 --no-deps` to attribute workspace-local source paths to
packages. That last input is not optional: Cargo passes *relative* source paths for local crates and
a package name need not match its directory, so bevy has several distinct packages in directories all
named `macros` and ripgrep's binary lives in `crates/core` while the package is `ripgrep`. Guessing
from the path silently dropped ten of bevy's units in an earlier attempt.

Commits are pinned in the table at the top. Absolute times will differ on other hardware; the ratios
— critical path against total work, and the direction of the job-count error — are what should
reproduce.
