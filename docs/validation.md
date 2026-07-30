# Validation against real builds

The model in [model.md](model.md) is only worth anything if it reproduces builds that actually
happened. This document records what was measured, including where the model is wrong.

Every figure here was produced by the code in this repository, by
`cargo run --release --example validate -p cargo-critpath -- <capture-root>`, over captured builds
reproducible with the commands in the last section.

## Method

Four projects, chosen for different graph shapes.

| project | commit | units | edges | shape |
| --- | --- | --- | --- | --- |
| ripgrep | `e89fff8` | 54 | 89 | small binary, shallow |
| async-graphql | `82cd1f1` | 119 | 201 | proc-macro heavy |
| rust-analyzer | `12c3381` | 308 | 893 | deep workspace |
| bevy | `c6f634c` | 419 | 1513 | wide workspace |

Each was built three times per configuration, `cargo clean` before every run, `--offline`, dev
profile, at `-j 10` and at `-j 2`. Machine: Apple M1 Pro class, 10 cores, macOS, `rustc 1.95.0`.

Predictions are compared against the **compilation span** - last unit end minus first unit start -
not Cargo's reported total, which includes resolution and fetch time outside the unit model. On a
cold registry that difference was 28 s of a 32.5 s "build". Figures are the median of three runs
unless per-run values are shown.

## The graph is checked against the build's own record of itself

Cargo's timing report notes, for each unit, which units its completion unblocked. Those notes are
incomplete - they name only whichever dependency finished last - but every pair in them is a real
edge, so they are a one-directional check on the parser, independent of anything the model believes.

| project | unblock events recorded | present in the parsed graph |
| --- | --- | --- |
| ripgrep | 35 | 35 |
| async-graphql | 74 | 74 |
| rust-analyzer | 203 | 203 |
| bevy | 299 | 299 |

All of them, on all 24 captured builds. The check has to be closure-aware to reach 100%: `syn` is
recorded as unblocking `bevy_encase_derive`, whose compiler invocation names no `syn` at all, which
is Cargo materialising the closure edges described in [model.md](model.md) section 3.1.

## Result 1: the model reproduces a build it has the weights for

For each run, the graph and weights come from **that run** and predict **that run's** span. This
isolates the schedule model from run-to-run variation in how long units take, which is result 4.

| project | `-j 10`, three runs | `-j 2`, three runs |
| --- | --- | --- |
| ripgrep | +1.0%, -1.1%, +0.4% | +0.8%, +0.5%, +0.8% |
| async-graphql | +0.3%, +0.2%, +0.0% | -0.6%, -0.4%, -0.1% |
| rust-analyzer | -0.4%, +0.2%, -0.5% | -1.9%, -2.1%, -2.0% |
| bevy | +0.2%, -0.1%, -0.2% | +1.3%, +1.0%, +0.2% |

**Every one of the 24 runs is within 2.1%**, and 18 of them within 1%. This is the claim the tool
rests on, and it is the only prediction it makes that is this accurate.

What is being tested is the **schedule model** - pipelining, the closure rule, the executor limit,
Cargo's cost-blind priority - not any ability to predict how long a compilation takes. Nothing here
attempts that.

The residual is not centred on zero at `-j 2` for rust-analyzer, where all three runs come in about
2% short. The simulation charges nothing for process startup or for the delay between a unit
becoming ready and Cargo actually starting it, and those costs are paid more often on a long build
with few executors.

## Result 2: at ten cores these builds are limited by their graphs

Medians of three runs at `-j 10`:

| project | total work | critical path | work / 10 | span | mean parallelism | CP / span |
| --- | --- | --- | --- | --- | --- | --- |
| ripgrep | 31540 ms | 5800 ms | 3154 ms | 5980 ms | 5.28 | 0.972 |
| async-graphql | 82710 ms | 18030 ms | 8271 ms | 18130 ms | 4.56 | 0.998 |
| rust-analyzer | 269150 ms | 59270 ms | 26915 ms | 60250 ms | 4.47 | 0.987 |
| bevy | 654880 ms | 151510 ms | 65488 ms | 152250 ms | 4.43 | 0.997 |

In every case the critical path exceeds total work divided by the core count, by a factor of 1.8 to
2.3. More cores could not have helped these builds. The critical path accounts for 97% to 99.8% of
the observed span - these builds very nearly *are* their critical paths.

Mean parallelism runs between 4.4 and 5.3 on a 10-core machine: roughly half the machine idle on
average, and it gets worse as the project gets bigger.

A number that circulates widely - that 32 cores yields an effective parallelism of about three -
could not be sourced and is not reproduced here. These are 10-core measurements and they gave 4.4 to
5.3. No figure in this repository was not measured here.

## Result 3: at two cores the same builds are not graph-limited

| project | CP / span at `-j 10` | CP / span at `-j 2` |
| --- | --- | --- |
| ripgrep | 0.972 | 0.493 |
| async-graphql | 0.998 | 0.538 |
| rust-analyzer | 0.987 | 0.467 |
| bevy | 0.997 | 0.502 |

At two cores the critical path is about half the span, the machine is the constraint, and
restructuring the graph would buy almost nothing. This is the distinction the tool reports as
`constraint: graph` against `constraint: machine`, and it is why it leads with that comparison
rather than with a recommendation.

It also means the thesis is a statement about machines with many cores. On a small machine these
graphs are fine.

## Result 4: weights do not survive a change in the job count

This is the largest error in the tool, and improving the simulator cannot fix it.

Per-unit durations are not invariant to how many compilations run at once. Summing the durations
Cargo reports, on the same commit:

| project | total unit work at `-j 10` | at `-j 2` | change |
| --- | --- | --- | --- |
| ripgrep | 31540 ms | 24220 ms | -23.2% |
| async-graphql | 82710 ms | 57760 ms | -30.2% |

The same compilations are a quarter to a third faster when fewer of them compete for memory
bandwidth. Result 1 shows the model is accurate at `-j 2` when given `-j 2` weights, so it is
carrying weights across the change that fails.

Taking the graph and weights from a `-j 10` build and predicting a real `-j 2` build:

| project | observed `-j 2` | predicted | error |
| --- | --- | --- | --- |
| ripgrep | 13520 ms | 18130 ms | +34.1% |
| async-graphql | 33040 ms | 51290 ms | +55.2% |
| rust-analyzer | 116190 ms | 141310 ms | +21.6% |
| bevy | 305130 ms | 380040 ms | +24.6% |

The area bound is itself violated: ripgrep's work-over-two from `-j 10` weights is 18080 ms and the
real `-j 2` build finished in 13520 ms. A bound computed from weights measured under different
contention is not a bound.

The direction is consistent. **Extrapolating from a low job count to a high one is optimistic; from
a high one to a low one is pessimistic.** This is why the executor scan leads with the knee, which is
decided by the critical path and so does not depend on the job count, rather than with absolute
times.

## Result 5: which parts of the model earn their place

Recomputing the critical path with every metadata edge treated as a full edge, that is, ignoring
pipelining:

| project | critical path | ignoring pipelining | overstatement |
| --- | --- | --- | --- |
| ripgrep | 5800 ms | 7990 ms | 27.8% |
| async-graphql | 18030 ms | 18640 ms | 3.0% |
| rust-analyzer | 59270 ms | 88600 ms | 32.8% |
| bevy | 151510 ms | 197880 ms | 23.6% |

Recomputing it with the closure requirement of [model.md](model.md) section 3.1 cleared on every
unit changes it by **0 ms on all twelve `-j 10` runs of all four projects**.

So pipelining is load-bearing and the closure rule, on these projects, is not. The 30% figure quoted
for the closure rule in section 3.1 comes from a fixture constructed to exhibit it. The rule stays
because it is what Cargo does - the unblock check above needs it to reach 100% - and because it can
bind on graphs these four do not cover. Nobody should believe it is what makes the tool accurate.

async-graphql is the outlier: pipelining is worth only 3% there, because its critical path runs
through two proc-macros, and a proc-macro cannot be pipelined past.

## Result 6: the critical path is stable on two projects and varies at the head on two

Three clean builds of the same commit at `-j 10`:

| project | identical path across 3 runs | path length | critical path lengths |
| --- | --- | --- | --- |
| ripgrep | yes | 6 | 5800 / 5970 / 5570 ms |
| async-graphql | yes | 12 | 19800 / 18000 / 18030 ms |
| rust-analyzer | no | 22 / 22 / 21 | 60150 / 59270 / 58840 ms |
| bevy | no | 21 / 20 / 21 | 164540 / 147450 / 151510 ms |

Where the path varies, it varies at the **head**, between interchangeable build-script chains: two
rust-analyzer runs began with `proc-macro2`'s build script and the third with `quote`'s. The tail -
everything a reader would act on - was identical in all three.

ripgrep's path in full, and it is short enough to act on:

    regex-syntax -> regex-automata -> bstr -> globset -> ignore -> ripgrep (bin)

async-graphql's, where the two proc-macros are why pipelining buys so little:

    proc-macro2 (build script, compile) -> (run) -> proc-macro2 -> quote -> syn
      -> serde_derive (proc-macro) -> serde -> bytes -> async-graphql-value
      -> async-graphql-parser -> async-graphql-derive (proc-macro) -> async-graphql

Six to twenty-two units out of 54 to 419 decide the whole build. That is why units are ranked by
contribution rather than by membership of the path: the ranking is stable where membership at the
margin is not.

## Result 7: run-to-run spread, and recommendations that fall inside it

Critical path across three runs of the same commit:

| project | spread | as a fraction |
| --- | --- | --- |
| ripgrep | 400 ms | 6.9% |
| async-graphql | 1800 ms | 10.0% |
| rust-analyzer | 1310 ms | 2.2% |
| bevy | 17090 ms | 11.3% |

The spread is not a fixed fraction - 2.2% on the largest project against 11.3% on another - so no
constant threshold would be right. The tool measures it when given several timing reports and
declines to guess when given one.

It matters because a saving can be smaller than it. Bevy's first run came in at 165030 ms and its
second at 147960 ms; first runs after a `cargo clean` appear to be systematically slower, so a single
measurement of a large project should not be trusted.

## Result 8: how the analysis scales

The quality bar this project set itself says a graph of several thousand units is analysed in
seconds. Measured with `cargo run --release --example scaling -p critpath-core`:

    units     edges   construct     analyse    simulate
     1000      3970     583.5us      86.8us      342.0us
     5000     19970       4.9ms      332.8us       5.4ms
    20000    119937      20.7ms        1.3ms      47.4ms
    50000    299937      35.3ms        2.0ms     221.2ms

The largest real project here has 419 units. At 50 000 - two orders of magnitude beyond it - the
critical path takes 2 ms and a full simulation 221 ms.

The two differ because they are different complexities. The analysis is a single topological pass and
is linear. The simulation is not: the `cargo-like` priority counts each unit's transitive
reverse-dependents, which is reachability counting, and the implementation propagates a 64-unit
bitmask backwards for `O((V/64)(V + E))`.

## What is not validated

- **Release builds.** Every number here is dev profile, where the compiler frontend dominates each
  unit and pipelining has less to give.
- **`cargo check`**, which is what most developers run in a loop, is a different graph and is refused
  by name rather than analysed. See [decisions.md](decisions.md) D15.
- **Platforms other than aarch64 macOS.**
- **Whether savings are realisable.** The tool predicts what a graph change would do to a schedule.
  It does not establish that the change is possible, nor that a real refactor would divide a crate's
  compile time the way the model assumes.
- **Anything across a `Cargo.lock` change.** Every measurement holds the lockfile fixed.

## Reproducing this

For each project, three times per job count:

    cargo clean
    cargo build --timings -v --offline -j <N> 2> verbose.log
    cp target/cargo-timings/cargo-timing.html .
    cargo metadata --format-version 1 --no-deps --offline > ../<project>-metadata.json

arranged as `<project>-j<N>-r<run>/` directories under one root, then:

    cargo run --release --example validate -p cargo-critpath -- <capture-root>

`cargo metadata` is not optional. Cargo passes *relative* source paths for workspace-local crates and
a package name need not match its directory: bevy has several distinct packages in directories all
named `macros`, and ripgrep's binary lives in `crates/core` while the package is `ripgrep`. Guessing
from the path silently dropped ten of bevy's units in an earlier attempt.

The captures are not committed - they run to hundreds of megabytes and are machine-specific.
Absolute times will differ on other hardware; the ratios, and the direction of the job-count error,
are what should reproduce.
