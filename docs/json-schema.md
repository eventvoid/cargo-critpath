# Machine-readable output

`cargo critpath --format json` writes one JSON object to stdout. This document is the contract, and
it is normative: where the implementation and this document disagree, the implementation is wrong.
The shape is required to be pinned against a fixture by a test, so that an accidental change fails
the build rather than a consumer's script.

## Versioning

The top-level `schema_version` is an integer, currently **1**.

- Adding a new optional field, or a new member to an array of objects, does **not** bump it.
  Consumers must ignore fields they do not know.
- Removing a field, renaming one, changing a type, or changing the meaning of an existing field
  bumps it.
- The tool's own version is reported separately as `tool_version` and follows semver
  independently. A patch release of the tool will never change `schema_version`.

Consumers should check `schema_version` and refuse a version they were not written for.

## Units of measurement

Every duration field ends in `_ms` and is an integer count of milliseconds. There are no
floating-point durations anywhere in the output. Cargo reports two decimal places of seconds, so
the underlying resolution is 10 ms; the values are exact integers, not rounded floats.

Every field that can be absent is present as `null` rather than omitted, except where stated.

## Top level

```json
{
  "schema_version": 1,
  "tool_version": "0.1.0",
  "input": { },
  "graph": { },
  "baseline": { },
  "critical_path": [ ],
  "units": [ ],
  "scenarios": [ ],
  "diagnostics": [ ]
}
```

### `input`

Describes the build the analysis was derived from. Present so that a number in a report can be
traced back to the build that produced it.

| Field | Type | Meaning |
| --- | --- | --- |
| `cargo_version` | string or null | always `null` today: the timing report's header carries `rustc`'s version but not Cargo's, and the tool does not run `cargo --version` to find out |
| `rustc_version` | string | as reported by the timing report header |
| `host` | string | host triple |
| `target` | string | target triple |
| `profile` | string | `dev`, `release`, or a custom profile name |
| `jobs` | integer | the `-j` value the observed build ran with |
| `observed_max_concurrency` | integer | peak simultaneous units Cargo actually reached |
| `units_total` | integer | units in the build |
| `units_fresh` | integer | of those, units that were cached. Analysis requires this to be 0 |
| `compilation_span_ms` | integer | last unit end minus first unit start |
| `reported_total_ms` | integer | Cargo's own reported total for the build |
| `excluded_prefix_ms` | integer | `reported_total_ms` minus the compilation span: fetches, resolution, and anything else outside the unit model |

`excluded_prefix_ms` is reported rather than folded in. A build that spent 28 s downloading and
4.7 s compiling is not a 32.5 s compilation problem, and the output must not let anyone believe it
is.

### `graph`

| Field | Type | Meaning |
| --- | --- | --- |
| `units` | integer | node count |
| `edges` | integer | edge count |
| `metadata_edges` | integer | of those, edges satisfied by metadata rather than full completion |
| `total_work_ms` | integer | sum of all unit durations |

### `baseline`

The analysis of the graph as it is.

| Field | Type | Meaning |
| --- | --- | --- |
| `executors` | integer | the `J` the analysis was run for |
| `critical_path_ms` | integer | `CP`; holds for any scheduling policy |
| `area_bound_ms` | integer | `total_work_ms / executors`, rounded up |
| `lower_bound_ms` | integer | `max(critical_path_ms, area_bound_ms)` |
| `simulated_makespan_ms` | integer | the list-scheduler estimate |
| `policy` | string | the simulator's priority rule: `cargo-like` (the default, reproducing Cargo's cost-blind ordering) or `longest-path` |
| `constraint` | string | `graph` when `critical_path_ms >= area_bound_ms`, else `machine` |
| `mean_parallelism` | number | `total_work_ms / simulated_makespan_ms`, to two decimals |

`constraint` is the field a CI script should branch on. When it is `machine`, the graph is not
what is slowing the build and the scenario list is not worth acting on.

`mean_parallelism` is the only non-integer numeric field in the output. It is derived, and a
consumer that wants exactness should compute it from the two integers itself.

### `critical_path`

An ordered array, from a unit with no dependencies to the unit that attains `CP`.

```json
{
  "unit": 12,
  "label": { "package": "syn", "version": "2.0.90", "target": "lib" },
  "kind": "lib",
  "enter_ms": 0,
  "exit_ms": 2570,
  "contribution_ms": 1430,
  "incoming_edge_kind": null,
  "total_ms": 2570,
  "metadata_at_ms": 1430
}
```

`contribution_ms` is what this unit adds to `CP`: `metadata_at_ms` when the *outgoing* edge along
the path is a metadata edge, and `total_ms` when it is a full edge or the unit is last. The
contributions are required to sum to `critical_path_ms`.

`incoming_edge_kind` is `metadata`, `full`, or `null` for the first element.

`unit` is an index into the `units` array, stable within one document and meaningless across
documents.

### `units`

One entry per unit, ordered by unit index. Emitted only with `--include-units`, because on a large
workspace it is far larger than the rest of the document.

```json
{
  "unit": 12,
  "label": { "package": "syn", "version": "2.0.90", "target": "lib" },
  "kind": "lib",
  "total_ms": 2570,
  "metadata_at_ms": 1430,
  "earliest_start_ms": 0,
  "earliest_finish_ms": 2570,
  "latest_finish_ms": 2570,
  "slack_ms": 0,
  "on_critical_path": true,
  "dependencies": 4,
  "dependents": 7
}
```

`kind` is one of `lib`, `proc-macro`, `bin`, `test`, `build-script-compile`, `build-script-run`,
`other`. New kinds may be added without bumping `schema_version`, so a consumer must not assume
the set is closed.

### `scenarios`

One entry per counterfactual. All of them share this envelope:

```json
{
  "kind": "split-crate",
  "target": { "package": "syn", "version": "2.0.90", "target": "lib" },
  "exact": false,
  "fraction_swept": { "low": 0.25, "high": 0.75 },
  "overhead_ms": 0,
  "assumptions": [
    "the division of work between the halves is swept from 0.25 to 0.75; no single division is assumed",
    "each half pays the per-crate overhead given by overhead_ms, which defaults to zero because no measurement of it exists",
    "dependent assignment: optimistic and pessimistic bounds coincide at an even division"
  ],
  "critical_path_ms": { "low": 3100, "high": 4200 },
  "saving_ms": { "low": 0, "high": 1100 },
  "simulated_makespan_ms": { "low": 3400, "high": 4600 },
  "anomaly": false,
  "notes": [ ]
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `kind` | string | `split-crate`, `remove-edge`, `drop-dependency`, `drop-feature`, `set-executors` |
| `target` | object or null | what the scenario acts on; shape depends on `kind` |
| `exact` | boolean | `true` only when the resulting graph is genuinely known, which is the case for `drop-dependency`, `drop-feature` and `remove-edge`, and never for `split-crate` |
| `fraction_swept` | object or null | for `split-crate`, the range of work divisions the interval spans. Null for other kinds |
| `overhead_ms` | integer or null | the fixed per-crate cost charged to each half of a split. Defaults to 0: no measurement of this quantity exists, so the tool does not invent one, and a caller who has measured it can supply it |
| `assumptions` | array of string | every assumption the numbers rest on, in prose. Never empty when `exact` is `false` |
| `critical_path_ms` | interval | `CP` after the change |
| `saving_ms` | interval | baseline `critical_path_ms` minus the scenario's. May be negative |
| `simulated_makespan_ms` | interval | makespan after the change |
| `anomaly` | boolean | `true` when the simulated makespan moved opposite to `CP`, which greedy list scheduling permits |
| `notes` | array of string | anything else a reader needs, such as a saving below the noise floor |

An **interval** is `{"low": <integer>, "high": <integer>}` with `low <= high`. When a scenario is
`exact`, `low` and `high` are equal — the field is still an interval so that consumers need only
one code path.

`saving_ms.low` may be negative. It is not clamped. A negative saving is information, and hiding
it would make the tool's confident-sounding numbers less trustworthy, not more.

### `diagnostics`

Machine-readable warnings and refusals. Empty array when there are none.

```json
{ "severity": "warning", "code": "saving-below-noise-floor",
  "message": "estimated saving of 300ms is smaller than the 400ms run-to-run spread measured across the supplied timing reports" }

{ "severity": "warning", "code": "noise-floor-unknown",
  "message": "only one timing report was supplied, so the run-to-run spread could not be measured and no saving can be checked against it" }
```

`severity` is `warning`. It is the only value emitted today, because anything bad enough to stop the
analysis is refused with a non-zero exit rather than reported as a diagnostic. Further severities may
be added without bumping `schema_version`, so a consumer must tolerate one it does not know. `code`
is a stable kebab-case identifier and is the field to
match on; `message` is prose and may change in any release. Codes may be added without bumping
`schema_version`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | analysis completed |
| 1 | usage error: bad arguments, unreadable path |
| 2 | input rejected: fresh units present, no timing report, unparseable report, empty graph, or an input that is not UTF-8 |
| 3 | analysis completed and a `--fail-if` threshold was exceeded |

Exit code 3 exists so a CI job can distinguish "the build graph regressed past my threshold" from
"the tool could not run". A tool that returns 1 for both forces the script to parse prose.

## Stability of ordering

Arrays are ordered deterministically, and the ordering is part of the contract:

- `units` by unit index, which is assigned in a canonical order derived from unit identity.
- `critical_path` from source to the attaining unit.
- `scenarios` by descending `saving_ms.high`, then by `kind`, then by target label. Ranking is
  part of the output's meaning, so it is specified rather than incidental.
- `diagnostics` by severity, most serious first, then by `code`. With only `warning` emitted today
  this reduces to ordering by `code`.

No array order depends on hash-map iteration.
