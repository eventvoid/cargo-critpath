# The schedule model

This document defines what `cargo-critpath` computes, and states its invariants and its known
systematic errors. It was written before the implementation, and the property tests cite the
invariant numbers in section 7.

## 1. Unit

A **unit** is one indivisible piece of scheduled work: one `rustc` invocation, or one execution
of a build-script binary. It is the node type of the graph.

    Unit {
        label:                 UnitLabel,       // package, version, target — for display only
        kind:                  UnitKind,
        total:                 Millis,          // wall time of the whole unit
        metadata_at:           Option<Millis>,  // offset from start at which metadata is emitted
        requires_full_closure: bool,            // see section 3.1
    }

`metadata_at` is `Some` exactly when the unit is pipelined — when Cargo invokes `rustc` with an
`--emit` list containing `metadata`. Requirement: `metadata_at <= total`.

Times are integer milliseconds, not floating point, so that comparisons, sorting and totals are
exact and the output is reproducible bit for bit.

## 2. Edge

An edge is directed producer to consumer and carries the kind of readiness the consumer needs:

    Edge { from, to, kind }
    EdgeKind = Metadata | Full

- **`Full`** — the consumer may start only when the producer has completely finished.
- **`Metadata`** — the consumer may start when the producer has emitted its metadata, that is,
  after `metadata_at` of the producer's own elapsed time.

A `Metadata` edge whose producer has no `metadata_at` is rejected at graph construction. It is a
typed error, not a silent coercion to `Full`.

Deciding which kind an edge has is the job of the build-system adapter, not of the core. For
Cargo the rule is derived from the compiler invocations themselves; see
[cargo-data-sources.md](cargo-data-sources.md).

## 3. Release time

For an edge `e = (D -> U)`, the moment it stops blocking `U`:

    release(e) = start(D) + metadata_at(D)    if kind(e) = Metadata
    release(e) = start(D) + total(D)          if kind(e) = Full

and the earliest `U` could begin:

    ready(U) = max over incoming e of release(e),   or 0 if U has no incoming edges

## 3.1 Units that link need their whole closure, not just their neighbours

Direct edges are not the whole constraint. A unit that produces a *linked* artifact — a binary,
a test, a proc-macro, a `cdylib`, `dylib` or `staticlib` — cannot run until every unit in its
**transitive** dependency closure is completely finished, not merely its immediate
dependencies. Such a unit has `requires_full_closure = true`.

The reason is that pipelining lets a library's `.rlib` be produced from its dependencies'
*metadata*, so an `.rlib` can exist on disk while the `.rlib` of something it depends on does
not yet. A linker needs all of them.

This was measured rather than assumed. Three libraries and a binary, where `biga` is large and
`smallb` and `cbin` are trivial, and `cbin` depends only on `smallb`:

    biga    start 0.05  metadata at 0.77  full end 1.24
    smallb  start 0.77  -> released by biga's metadata, as expected
    smallb            full end 0.82
    cbin    start 1.25  -> NOT 0.82

`cbin` waited for `biga`, which it does not name as a dependency. A model built from direct
edges alone would have predicted `cbin` starting at 0.82 and would have understated this
critical path by about 30%.

The rule applies to proc-macros and not to ordinary libraries. With `biga <- mid` and both
`pmc` (a proc-macro) and `toplib` (a library) depending only on `mid`:

    biga    metadata at 1.02   full end 1.48
    mid     metadata at 1.05   full end 1.09
    toplib  start 1.05   -> released by mid's metadata; no closure requirement
    pmc     start 1.49   -> waited for biga's full end; closure requirement applies

So `requires_full_closure` is a property of the consumer, and it reaches past its neighbours.

**It does not need a transitive closure to be materialised.** Define, over units in topological
order,

    maxFullEft(U) = max over direct dependencies D of  max( eft(D), maxFullEft(D) )
                    0 if U has no dependencies

which is the latest finish time anywhere in `U`'s transitive dependency closure. Then

    est(U) = maxFullEft(U)                                if requires_full_closure(U)
    est(U) = max over incoming e of release(e)            otherwise

This is a single pass in topological order and stays `O(V + E)`; no closure is enumerated and no
`O(V^2)` edge set is built. Keeping it derived rather than materialised also means the
counterfactual transformations of section 9 stay correct automatically: when a transformation
changes the graph, the closures it implies change with it, which materialised edges would not.

Verified against the second fixture: the recurrence puts `toplib` at 0.80, `pmc` at 1.24 and
`mid` at 0.78 relative to `biga`'s start, against observed 0.81, 1.25 and 0.79 — inside the
10 ms resolution of the input.

## 4. Critical path

With unlimited executors every unit starts exactly at its earliest start time. Define, in
topological order,

    est(U) = as given in section 3.1, which is ready(U) for an ordinary unit
             and maxFullEft(U) for a unit that requires its full closure
    eft(U) = est(U) + total(U)
    CP     = max over all U of eft(U)

`CP` is the **critical path length**: the longest weighted path under the edge semantics of
section 3. It depends only on the graph and the weights — not on the executor count and not on
any scheduling policy. That is why it is the number the tool leads with.

The **critical path** is the chain recovered by tracing back from the unit attaining `CP`,
following at each step the incoming edge that determined `est`, until a unit with no
dependencies is reached.

A `Metadata` edge lets a path traverse a unit contributing only `metadata_at` rather than its
full `total`. Defining `CP` in terms of `release` rather than `total` is what keeps the critical
path from being systematically overstated on any build that uses pipelining, which is every
build that compiles more than one library.

## 5. Bounded-executor schedule

Parameter `J >= 1`, the number of executors.

The simulator is a non-preemptive list scheduler:

1. Time starts at 0, all units unstarted.
2. A unit is **ready** at time `t` when every incoming edge has `release(e) <= t`.
3. While an executor is free and a ready unit exists, the highest-priority ready unit starts.
4. A started unit occupies exactly one executor for exactly `total`, without preemption or
   migration.
5. `makespan(J)` is the time the last unit finishes.

**Priority.** Ties have to be broken deterministically or the output is not reproducible. Unit
ordering is canonical and never derived from hash iteration order.

Two policies are offered, because they answer different questions.

- `cargo-like` (**default**) reproduces what Cargo actually does. Cargo's scheduler is
  *cost-blind*: it assigns every unit a fixed cost of 100 and sets a unit's priority to its own
  cost plus the cost of all of its transitive reverse-dependencies, which reduces to
  `1 + (number of transitive reverse-dependents)`. It knows nothing about how long anything
  takes. Reproducing this is the only way `makespan(J)` estimates Cargo's wall time rather than
  the wall time of a better build system.

- `longest-path` schedules the unit with the longest remaining path to a sink first. It is the
  standard critical-path heuristic and gives a *better* schedule than Cargo's. It is offered so
  that the gap between the two can be read off: that gap is how much is lost to Cargo's ordering
  alone, as opposed to the shape of the graph.

**Cargo's real ordering is not deterministic.** Having selected the maximum priority, Cargo
breaks ties by hash-map iteration order. Two runs of the same build can therefore schedule
differently, which puts a floor on how accurately any simulator can predict a single run. The
simulator breaks the same ties by unit identity so that its own output is reproducible, and the
resulting difference is one of the reasons `makespan(J)` is labelled an estimate while the bounds
in section 6 are guarantees.

## 6. Two policy-independent bounds

For every scheduling policy and every `J`:

    makespan(J) >= CP                      (precedence bound)
    makespan(J) >= total_work / J          (area bound), total_work = sum of total(U)

so with `LB(J) = max(CP, total_work / J)`, every schedule satisfies `makespan(J) >= LB(J)`.

`LB(J)` is reported alongside the simulated value, because the comparison between its two terms
is the whole diagnosis:

- `CP` dominates: the graph is the constraint. More cores cannot help. This tool is useful.
- `total_work / J` dominates: the machine is the constraint. Restructuring the graph will not
  help much, and the tool says so rather than inventing a recommendation.

## 7. Invariants

The property tests cite these numbers.

**I1 — Determinism.** The same graph yields identical output across runs and platforms, with no
dependence on hash iteration order.

**I2 — Precedence respected.** For every edge `e = (D -> U)` in a produced schedule,
`start(U) >= release(e)`.

**I3 — Capacity respected.** At no instant are more than `J` units running.

**I4 — Non-preemption.** Each unit occupies one executor over exactly
`[start(U), start(U) + total(U))`.

**I5 — Precedence bound.** `makespan(J) >= CP` for all `J`.

**I6 — Area bound.** `makespan(J) >= total_work / J` for all `J`.

**I7 — Unlimited executors reach the critical path.** For `J >= unit count`,
`makespan(J) == CP` exactly.

**I8 — Monotonicity in J.** `makespan(J+1) <= makespan(J)`. This holds for the scheduler as
specified only because the priority is a fixed total order that does not depend on `J`; it is
asserted as a property test precisely because it is easy to break.

**I9 — Slack non-negative, zero on the critical path.** With `lft(U)` the latest finish that
does not increase `CP`, `slack(U) = lft(U) - eft(U) >= 0`, and `slack(U) = 0` for every `U` on a
critical path.

**I10 — `CP` is independent of `J`.** Computing `CP` never consults `J`.

**I11 — Single unit.** `CP = total(U)` and `makespan(J) = total(U)` for all `J >= 1`.

**I12 — Empty graph.** `CP = 0`, `makespan = 0`, no panic and no division by zero.

**I13 — Metadata edges cannot lengthen a path.** Reclassifying any `Full` edge as `Metadata`
cannot increase `CP`, because `metadata_at <= total`.

**I14 — Weight monotonicity.** Increasing any `total(U)` cannot decrease `CP`.

**I16 — A closure requirement can only delay.** Setting `requires_full_closure` on any unit
cannot decrease `CP`, and clearing it on any unit cannot increase `CP`.

**I17 — A closure requirement subsumes direct edges.** For a unit `U` with
`requires_full_closure`, `est(U) >= eft(D)` for every `D` in its transitive dependency closure,
not merely for its direct dependencies.

**I18 — The recurrence equals the enumerated closure.** `maxFullEft(U)` computed by the
recurrence of section 3.1 equals `max over D in transitiveDeps(U) of eft(D)`, computed by
explicit enumeration. This is a differential test against a deliberately naive implementation,
because the recurrence is the part of the model most likely to be subtly wrong.

**I15 — Chain and fan closed forms.** For a chain of `n` units of weight `w` joined by `Full`
edges, `CP = n*w`. For one source fanning out to `n` sinks, `CP = w_source + max w_sink`. The
tests compute these by hand rather than by calling the code under test.

### An anomaly that is deliberately not an invariant

**Removing an edge can increase `makespan(J)`.** This is a Graham scheduling anomaly and a
genuine property of greedy list scheduling, not a defect. It cannot happen to `CP`, which is
monotone under edge removal; only the bounded-executor simulation is affected.

So a counterfactual never reports a negative saving as though it were a modelling error, and
never silently clamps one to zero. Where the simulated makespan moves the wrong way, the
reported interval is anchored on the `CP` change, which is monotone, and the anomaly is
disclosed in the output.

## 8. Known systematic errors

These are stated here, and repeated in the README, so that a user meets them before being
surprised by them.

1. **Fixed weights under a changed executor count. This is the largest error in the model.**
   Per-unit durations come from one real build at one `-j` value, and they are not invariant to it.
   Measured on clean offline builds of the same commit, median of three runs, summing the per-unit
   durations Cargo reports: `ripgrep`'s total unit work fell 23.2% and `async-graphql`'s fell 30.2%
   when going from `-j 10` to `-j 2`. The same compilations are simply faster when fewer of them
   compete for memory bandwidth.

   Consequently, taking the graph and weights from a `-j 10` build and predicting a real `-j 2`
   build over-predicted by 34.1% and 55.2% respectively, while the same simulator was within 0.2%
   and 1.3% at the executor count the weights came from. The simulator is accurate; reusing weights
   across a change in contention is not.

   The direction is consistent: **extrapolating from a low executor count to a high one is
   optimistic, and from a high one to a low one is pessimistic.** This is why the executor scan
   leads with the knee — the point beyond which the critical path dominates — which is decided by
   `CP` and therefore does not depend on the executor count, rather than with absolute times.

2. **One unit occupies one executor.** Cargo hands `rustc` jobserver tokens, and a `rustc` process
   holds one implicit token for its lifetime while acquiring more for parallel codegen. So `J` is a
   bound on a token pool, not a count of concurrent compilations. The direction of the error is
   known only per-effect, and the two effects have opposite signs. A `rustc` holding extra tokens
   means fewer compilations run at once than `J` suggests, making the simulated makespan too short.
   But build-script executions are not compiler processes and take no compile token, so real
   concurrency can exceed `J`: a clean `ripgrep` build at `-j 10` reported a peak concurrency of
   12, with five build-script runs active at once. Capping the simulation at `J` then makes it too
   long. **The net direction is not known in general**, so the observed peak concurrency is
   reported next to `J` rather than the discrepancy being assumed away. The measured durations
   already contain whatever internal parallelism the observed build received, which makes the
   baseline self-consistent and the extrapolation approximate.

3. **Non-compilation time is excluded.** Registry index updates, crate downloads and time spent
   in an external linker are outside the unit model. Predictions are compared against the
   compilation span, and any excluded prefix is reported separately rather than folded in.

4. **A split crate cannot be modelled exactly.** Which half each dependent actually needs is not
   knowable without performing the split, which is why section 9.1 produces an interval.

5. **Measurement noise.** One build's per-unit times vary from run to run. Savings smaller than
   that spread are not actionable and are not presented as if they were.

## 9. Counterfactual transformations

Each transformation produces a new graph, and the metrics of sections 4 to 6 are recomputed on
it. The result is reported as a **difference**, and where the transformation is underdetermined,
as an **interval with its assumptions named** — never as a single confident number.

### 9.1 Split a crate

Unit `U` of weight `w` becomes `U_a` and `U_b`. Two things are unknown: how the work divides,
and which dependents need which half.

- Work division is a declared parameter `f` (default one half), giving `total(U_a) = f*w` and
  `total(U_b) = (1-f)*w`. Both halves inherit `U`'s incoming edges.
- The dependents are bounded rather than guessed:
  - **Optimistic bound** — each dependent needs exactly one half. The best a split could do.
  - **Pessimistic bound** — every dependent still needs both halves. The split buys nothing
    structurally, and only the pipelining boundary moves.

A split is also not free: two crates re-parse shared imports and each pays a fixed per-crate
cost. A per-unit overhead is added to each half, and it is a declared parameter rather than a
hidden constant.

**The dependent bounds are often degenerate, and saying otherwise would overstate what they
bracket.** At `f = 1/2` the two halves finish at the same instant, so which dependents attach to
which half cannot change any downstream start time, and the optimistic and pessimistic answers
coincide exactly. Measured: splitting `regex-automata` on `ripgrep`'s critical path gave a 300 ms
saving under both assumptions, and splitting `async_graphql` gave 3395 ms under both. The second
case is degenerate for a further reason — it is a sink with no dependents at all.

The dependent interval only opens up when the halves are unequal, or when they inherit different
incoming constraints. So the dominant uncertainty in a split is **not** which dependents need which
half. It is `f` itself: how the work actually divides, which is exactly what nobody knows before
doing the split.

The reported interval therefore spans a **sweep over `f`** as well as the dependent assumption, and
the output names both. Presenting an optimistic/pessimistic pair as though it bracketed the real
uncertainty, when at the default `f` it collapses to a single number, would be a confident-looking
interval that hides the variable that matters.

### 9.2 Break a dependency edge

Remove the edge and recompute. The `CP` change is monotone and anchors the answer; the simulated
makespan change is reported alongside it, with the section 7 anomaly disclosed if it moves the
wrong way.

### 9.3 Drop a dependency or a feature

Remove the target package's units, then remove every unit that is no longer reachable from the
build's roots, and recompute. This is the one transformation whose *structure* is exact: the
graph after dropping a dependency really is known. The weights of surviving units may still
shift if the feature change alters what they compile, and that caveat is reported.

### 9.4 Change the executor count

Recompute `CP` and `LB(J)` across a range of `J` and report the **knee**: the smallest `J` at which
`CP >= total_work / J`, beyond which the precedence bound dominates and more executors cannot help.

The knee is the number to act on. It is determined by `CP`, which does not depend on `J`, against
total work, which rescales under contention but does not reorder — so it survives the 23% to 30%
weight error measured in systematic error 1, whereas an absolute makespan does not.

Absolute `makespan(J)` values are still reported across the range, and every one of them away from
the executor count the weights were measured at is marked unreliable, with the direction of the
error given.

## 10. Constraints on the core

- The core performs no I/O and knows nothing about Cargo, `rustc`, file paths or JSON. Adapters
  hand it a graph. Supporting another build system means writing another adapter, not touching
  the core.
- No `unwrap`, `expect`, `panic!` or unchecked indexing. Errors are typed.
- Critical path, slack and one simulation are each `O(V + E)`.
