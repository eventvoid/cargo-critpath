# The schedule model

This document defines what `cargo-critpath` computes, and states its invariants and its known
systematic errors. It was written before the implementation, and the property tests cite the
invariant numbers in section 7.

## 1. Unit

A **unit** is one indivisible piece of scheduled work: one `rustc` invocation, or one execution
of a build-script binary. It is the node type of the graph.

    Unit {
        label:       UnitLabel,       // package, version, target — for display only
        kind:        UnitKind,
        total:       Millis,          // wall time of the whole unit
        metadata_at: Option<Millis>,  // offset from unit start at which rmeta is emitted
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

## 4. Critical path

With unlimited executors every unit starts exactly at `ready(U)`. Define

    est(U) = max over incoming e of release(e) using est,   0 if no incoming
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

**Priority.** Ties have to be broken deterministically or the output is not reproducible. The
default priority is *longest remaining path to a sink first*, with unit identity as the final
tiebreak. Unit ordering is canonical and never derived from hash iteration order.

The priority is a property of this simulator, not of Cargo. Cargo's real queue order is not
critical-path-first, so `makespan(J)` is an estimate of Cargo's wall time, whereas the bounds in
section 6 are guarantees.

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

1. **Fixed weights under a changed executor count.** Per-unit durations come from one real build
   at one `-j` value. Re-simulating at a different `J` reuses those durations, but a real
   `rustc`'s internal codegen parallelism and its competition for memory bandwidth both change
   with load. Predictions for `J` far from the measured value therefore drift, and in a known
   direction: they are optimistic for large `J`.

2. **One unit occupies one executor.** Cargo hands `rustc` jobserver tokens and a single `rustc`
   can hold more than one for parallel codegen, so "J executors" is an abstraction over a token
   pool. The measured durations already contain whatever internal parallelism the observed build
   received, which makes the baseline self-consistent and the extrapolation approximate.

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

The answer is the interval between those bounds with the assumption printed. No point estimate
is offered, because the tool cannot know it.

A split is also not free: two crates re-parse shared imports and each pays a fixed per-crate
cost. A per-unit overhead is added to each half, and it is a declared parameter rather than a
hidden constant.

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

Recompute `makespan(J)` and `LB(J)` across a range of `J`, subject to systematic error 1 above,
which is stated in the output rather than buried in a footnote.

## 10. Constraints on the core

- The core performs no I/O and knows nothing about Cargo, `rustc`, file paths or JSON. Adapters
  hand it a graph. Supporting another build system means writing another adapter, not touching
  the core.
- No `unwrap`, `expect`, `panic!` or unchecked indexing. Errors are typed.
- Critical path, slack and one simulation are each `O(V + E)`.
