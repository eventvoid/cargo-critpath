//! Everything a report says, computed once and rendered twice.
//!
//! The two renderers — [`crate::report`] and [`crate::json`] — read this and nothing else, so the
//! human report and the machine document can never disagree about a number. Every decision about
//! *what* to report lives here; the renderers only decide how it looks.

use core::num::{NonZeroU32, NonZeroU64};
use std::collections::BTreeSet;

use critpath_core::{
    analyze, apply, area_bound, simulate, Analysis, BuildGraph, EdgeKind, Millis, Policy,
    SplitDependents, Transform, UnitId, UnitKind,
};

use crate::input::{BuildInput, BuildMeta, Diagnostic, Severity};

/// The divisions of a crate's work the split scenario is swept over.
///
/// The endpoints of the sweep are named in the output, and the endpoints of the *interval* — a
/// fraction of nothing and a fraction of everything — are deliberately excluded: they describe a
/// crate that was not split.
const SWEEP: [(u64, u64); 5] = [(1, 4), (1, 3), (1, 2), (2, 3), (3, 4)];

/// The lowest and highest division in [`SWEEP`], in hundredths.
const SWEEP_BOUNDS: (u32, u32) = (25, 75);

/// A closed range of durations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Range {
    /// The shortest value in the range.
    pub low: Millis,
    /// The longest value in the range.
    pub high: Millis,
}

/// A closed range of savings, which may be negative: a change can lengthen a build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Saving {
    /// The least this change could save.
    pub low: i64,
    /// The most this change could save.
    pub high: i64,
}

/// Enough of a unit's identity to name it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Label {
    /// The package name.
    pub package: String,
    /// The package version.
    pub version: String,
    /// The target within the package.
    pub target: String,
}

impl Label {
    fn of(graph: &BuildGraph, id: UnitId) -> Self {
        match graph.unit(id) {
            Some(unit) => Self {
                package: unit.package.clone(),
                version: unit.version.clone(),
                target: unit.target.clone(),
            },
            None => Self {
                package: String::new(),
                version: String::new(),
                target: String::new(),
            },
        }
    }

    /// The unit as one line: `package version (target)`.
    pub(crate) fn display(&self) -> String {
        format!("{} {} ({})", self.package, self.version, self.target)
    }
}

/// Counts and totals over the graph as it is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GraphFacts {
    /// How many units.
    pub units: usize,
    /// How many edges.
    pub edges: usize,
    /// Of those, how many are satisfied by metadata rather than by a completed dependency.
    pub metadata_edges: usize,
    /// The sum of every unit's duration.
    pub total_work: Millis,
}

/// The analysis of the graph as it is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Baseline {
    /// The executor count the analysis was run for.
    pub executors: NonZeroU32,
    /// The critical path length, which holds under any scheduling policy.
    pub critical_path: Millis,
    /// Total work divided between the executors, rounded up.
    pub area_bound: Millis,
    /// The larger of the two bounds.
    pub lower_bound: Millis,
    /// The list scheduler's estimate.
    pub makespan: Millis,
    /// The priority rule the estimate used.
    pub policy: Policy,
    /// Whether the critical path is what holds the build back, rather than the machine.
    pub graph_constrained: bool,
    /// Total work over the simulated makespan, in hundredths.
    pub mean_parallelism: u64,
}

/// One unit of the critical path, with what it contributes to the path's length.
#[derive(Clone, Debug)]
pub(crate) struct Step {
    /// The unit's index.
    pub unit: usize,
    /// What it is.
    pub label: Label,
    /// What sort of work it performs.
    pub kind: UnitKind,
    /// When the path reaches it.
    pub enter: Millis,
    /// When it finishes.
    pub exit: Millis,
    /// What it adds to the path's length.
    pub contribution: Millis,
    /// What the step into this unit waited for, and `None` for the first unit.
    pub incoming: Option<EdgeKind>,
    /// The unit's whole duration.
    pub total: Millis,
    /// When it emits metadata, if it does.
    pub metadata_at: Option<Millis>,
}

/// Everything the analysis knows about one unit.
#[derive(Clone, Debug)]
pub(crate) struct UnitFacts {
    /// The unit's index.
    pub unit: usize,
    /// What it is.
    pub label: Label,
    /// What sort of work it performs.
    pub kind: UnitKind,
    /// Its whole duration.
    pub total: Millis,
    /// When it emits metadata, if it does.
    pub metadata_at: Option<Millis>,
    /// The earliest it could start.
    pub earliest_start: Millis,
    /// The earliest it could finish.
    pub earliest_finish: Millis,
    /// The latest it may finish without lengthening the build.
    pub latest_finish: Millis,
    /// The room between those two.
    pub slack: Millis,
    /// Whether it has no room at all.
    pub on_critical_path: bool,
    /// How many units it depends on.
    pub dependencies: usize,
    /// How many units depend on it.
    pub dependents: usize,
    /// What it adds to the reported critical path, and zero when it is not on it.
    pub contribution: Millis,
    /// Whether dividing it in two is a change anyone could make.
    pub splittable: bool,
}

/// What a counterfactual acts on.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    /// One unit of the graph.
    Unit(Label),
    /// The number of executors, which changes no unit and no edge.
    Executors(u32),
}

/// One counterfactual, with the interval it produced and the assumptions behind it.
#[derive(Clone, Debug)]
pub(crate) struct Scenario {
    /// The kind of change, as the machine-readable contract names it.
    pub kind: &'static str,
    /// What it acts on.
    pub target: Target,
    /// Whether the resulting graph is genuinely known.
    pub exact: bool,
    /// For a split, the range of divisions swept, in hundredths.
    pub fraction_swept: Option<(u32, u32)>,
    /// The fixed per-crate cost charged to each half of a split.
    pub overhead: Option<Millis>,
    /// Every assumption the numbers rest on.
    pub assumptions: Vec<String>,
    /// The critical path after the change.
    pub critical_path: Range,
    /// The baseline critical path minus the scenario's.
    pub saving: Saving,
    /// The simulated makespan after the change.
    pub makespan: Range,
    /// Whether the simulated makespan moved opposite to the critical path anywhere in the sweep.
    pub anomaly: bool,
    /// Anything else a reader needs.
    pub notes: Vec<String>,
}

/// Which way a simulated makespan for an executor count is wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    /// The weights were measured at this executor count.
    Measured,
    /// Above the count the weights came from, so the estimate is too short.
    Optimistic,
    /// Below the count the weights came from, so the estimate is too long.
    Pessimistic,
    /// The timing report did not record what the build ran with.
    Unknown,
}

/// One row of the executor scan.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScanPoint {
    /// The executor count.
    pub executors: NonZeroU32,
    /// Total work divided between them, rounded up.
    pub area_bound: Millis,
    /// The larger of that and the critical path.
    pub lower_bound: Millis,
    /// The list scheduler's estimate at this count.
    pub makespan: Millis,
    /// Whether the critical path dominates here.
    pub graph_constrained: bool,
    /// Whether this is the smallest count at which it does.
    pub is_knee: bool,
    /// Which way the estimate is wrong.
    pub direction: Direction,
    /// The executor count the weights were measured at, where the timing report recorded it.
    pub measured_at: Option<u32>,
}

/// What is known about the run-to-run spread of this build.
#[derive(Clone, Debug)]
pub(crate) enum Noise {
    /// Only one timing report was supplied, so nothing was measured.
    Unknown,
    /// Several were, and this is what they disagreed by.
    Measured {
        /// The longest critical path minus the shortest.
        spread: Millis,
        /// The critical path of each report, in the order they were given.
        samples: Vec<Millis>,
    },
}

impl Noise {
    /// The measured spread, or `None` when it could not be measured.
    pub(crate) fn spread(&self) -> Option<Millis> {
        match self {
            Self::Unknown => None,
            Self::Measured { spread, .. } => Some(*spread),
        }
    }
}

/// Everything one analysis produced.
#[derive(Clone, Debug)]
pub(crate) struct Findings {
    /// Where the numbers came from.
    pub input: BuildMeta,
    /// The shape of the graph.
    pub graph: GraphFacts,
    /// The analysis of the graph as it is.
    pub baseline: Baseline,
    /// The critical path, from a source to the unit that attains it.
    pub path: Vec<Step>,
    /// Every unit, in index order.
    pub units: Vec<UnitFacts>,
    /// Indices into `units`, by descending contribution to the critical path.
    pub ranked: Vec<usize>,
    /// The counterfactuals, ranked as the machine-readable contract specifies.
    pub scenarios: Vec<Scenario>,
    /// Indices into `units` of ranked contributors that no split can be offered for.
    pub unsplittable: Vec<usize>,
    /// The executor scan.
    pub scan: Vec<ScanPoint>,
    /// The smallest executor count at which the critical path dominates.
    pub knee: u32,
    /// What is known about the run-to-run spread.
    pub noise: Noise,
    /// Warnings and refusals, ordered by severity and then by code.
    pub diagnostics: Vec<Diagnostic>,
    /// How many ranked units and scenarios to show.
    pub top: usize,
}

/// Computes everything a report needs from one parsed build.
///
/// `samples` holds the critical path of every timing report that was supplied, in the order they
/// were given; the first is the build the rest of the analysis comes from.
pub(crate) fn assemble(
    build: &BuildInput,
    samples: &[Millis],
    executors: NonZeroU32,
    policy: Policy,
    top: usize,
) -> Findings {
    let graph = &build.graph;
    let analysis = analyze(graph);
    let ids: Vec<UnitId> = graph.units().map(|(id, _)| id).collect();

    let critical_path = analysis.critical_path_len();
    let area = area_bound(graph, executors);
    let schedule = simulate(graph, executors, policy);
    let makespan = schedule.makespan();
    let total_work = graph.total_work();

    let baseline = Baseline {
        executors,
        critical_path,
        area_bound: area,
        lower_bound: critical_path.max(area),
        makespan,
        policy,
        graph_constrained: critical_path >= area,
        mean_parallelism: hundredths(total_work, makespan),
    };

    let units = unit_facts(graph, &analysis, &ids);
    let mut ranked: Vec<usize> = (0..units.len())
        .filter(|i| units.get(*i).is_some_and(|u| u.contribution > Millis::ZERO))
        .collect();
    // Descending contribution, and on a tie the lower index, so the order never depends on the
    // order the units happened to be visited in.
    ranked.sort_by(|a, b| {
        let contribution = |i: &usize| units.get(*i).map_or(Millis::ZERO, |u| u.contribution);
        contribution(b).cmp(&contribution(a)).then(a.cmp(b))
    });

    let noise = match samples.len() {
        0 | 1 => Noise::Unknown,
        _ => {
            let low = samples.iter().copied().min().unwrap_or(Millis::ZERO);
            let high = samples.iter().copied().max().unwrap_or(Millis::ZERO);
            Noise::Measured {
                spread: high.saturating_sub(low),
                samples: samples.to_vec(),
            }
        }
    };

    let knee = knee_of(total_work, critical_path);
    let scan = executor_scan(
        graph,
        critical_path,
        executors,
        knee,
        policy,
        build.build.jobs,
    );

    let mut scenarios: Vec<Scenario> = Vec::new();
    let unsplittable: Vec<usize> = ranked
        .iter()
        .copied()
        .filter(|i| units.get(*i).is_some_and(|u| !u.splittable))
        .take(top)
        .collect();
    for index in ranked
        .iter()
        .copied()
        .filter(|i| units.get(*i).is_some_and(|u| u.splittable))
        .take(top)
    {
        let (Some(unit), Some(id)) = (units.get(index), ids.get(index).copied()) else {
            continue;
        };
        if let Some(scenario) = split_scenario(graph, &baseline, id, unit, &noise, policy) {
            scenarios.push(scenario);
        }
    }
    scenarios.extend(scan.iter().map(|point| executor_scenario(&baseline, point)));
    sort_scenarios(&mut scenarios);

    let mut diagnostics = build.diagnostics.clone();
    diagnostics.extend(noise_diagnostics(&noise, &scenarios, samples.len()));
    // Most serious first, then by code. `Severity` has one variant today, so this reduces to
    // ordering by code. A more serious severity added later must be declared *before* `Warning`,
    // because the derived ordering follows declaration order and this sort depends on it.
    diagnostics.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.code.cmp(b.code))
            .then(a.message.cmp(&b.message))
    });

    Findings {
        input: build.build.clone(),
        graph: GraphFacts {
            units: graph.len(),
            edges: graph.edge_count(),
            metadata_edges: metadata_edges(graph, &ids),
            total_work,
        },
        baseline,
        path: path_steps(graph, &analysis),
        units,
        ranked,
        scenarios,
        unsplittable,
        scan,
        knee,
        noise,
        diagnostics,
        top,
    }
}

fn metadata_edges(graph: &BuildGraph, ids: &[UnitId]) -> usize {
    ids.iter()
        .filter_map(|id| graph.dependencies(*id))
        .flatten()
        .filter(|dep| dep.kind == EdgeKind::Metadata)
        .count()
}

fn unit_facts(graph: &BuildGraph, analysis: &Analysis, ids: &[UnitId]) -> Vec<UnitFacts> {
    ids.iter()
        .filter_map(|id| {
            let unit = graph.unit(*id)?;
            Some(UnitFacts {
                unit: id.index(),
                label: Label::of(graph, *id),
                kind: unit.kind,
                total: unit.total,
                metadata_at: unit.metadata_at,
                earliest_start: analysis.earliest_start(*id).unwrap_or(Millis::ZERO),
                earliest_finish: analysis.earliest_finish(*id).unwrap_or(Millis::ZERO),
                latest_finish: analysis.latest_finish(*id).unwrap_or(Millis::ZERO),
                slack: analysis.slack(*id).unwrap_or(Millis::ZERO),
                on_critical_path: analysis.is_critical(*id),
                dependencies: graph.dependencies(*id).map_or(0, <[_]>::len),
                dependents: graph.dependents(*id).map_or(0, <[_]>::len),
                contribution: analysis.critical_contribution(*id).unwrap_or(Millis::ZERO),
                splittable: !matches!(
                    unit.kind,
                    UnitKind::BuildScriptCompile | UnitKind::BuildScriptRun
                ),
            })
        })
        .collect()
}

fn path_steps(graph: &BuildGraph, analysis: &Analysis) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    for id in analysis.critical_path() {
        let Some(unit) = graph.unit(*id) else {
            continue;
        };
        let contribution = analysis.critical_contribution(*id).unwrap_or(Millis::ZERO);
        // What the previous unit contributed says what the step out of it waited for: a unit
        // traversed over a metadata edge contributes less than the whole of itself. Consecutive
        // units on the path need not be joined by an edge at all, so reading the graph instead
        // would answer a different question.
        let incoming = steps.last().map(|previous: &Step| {
            if previous.contribution < previous.total {
                EdgeKind::Metadata
            } else {
                EdgeKind::Full
            }
        });
        steps.push(Step {
            unit: id.index(),
            label: Label::of(graph, *id),
            kind: unit.kind,
            enter: analysis.earliest_start(*id).unwrap_or(Millis::ZERO),
            exit: analysis.earliest_finish(*id).unwrap_or(Millis::ZERO),
            contribution,
            incoming,
            total: unit.total,
            metadata_at: unit.metadata_at,
        });
    }
    steps
}

/// The smallest executor count at which the critical path dominates the area bound.
///
/// Decided by the critical path and total work, neither of which depends on the executor count, so
/// it survives the weight error that makes an absolute makespan at another count unreliable.
fn knee_of(total_work: Millis, critical_path: Millis) -> u32 {
    let dominates = |j: NonZeroU32| critical_path >= total_work.div_ceil_by(NonZeroU64::from(j));
    let start = match NonZeroU64::new(critical_path.get()) {
        Some(path) => u32::try_from(total_work.div_ceil_by(path).get()).unwrap_or(u32::MAX),
        // Nothing takes any time, so one executor already attains the bound.
        None => 1,
    };
    let mut candidate = start.max(1);
    loop {
        let Some(j) = NonZeroU32::new(candidate) else {
            return 1;
        };
        if dominates(j) || candidate == u32::MAX {
            return candidate;
        }
        candidate = candidate.saturating_add(1);
    }
}

fn scan_points(executors: NonZeroU32, knee: u32) -> Vec<NonZeroU32> {
    let cap = 16
        .max(executors.get().saturating_mul(2))
        .max(knee.saturating_add(2));
    let mut points: BTreeSet<u32> = BTreeSet::new();
    let mut doubling = 1u32;
    while doubling <= cap {
        points.insert(doubling);
        let Some(next) = doubling.checked_mul(2) else {
            break;
        };
        doubling = next;
    }
    points.insert(executors.get());
    points.insert(knee.max(1));
    points.into_iter().filter_map(NonZeroU32::new).collect()
}

fn executor_scan(
    graph: &BuildGraph,
    critical_path: Millis,
    executors: NonZeroU32,
    knee: u32,
    policy: Policy,
    measured_at: Option<u32>,
) -> Vec<ScanPoint> {
    scan_points(executors, knee)
        .into_iter()
        .map(|j| {
            let area = area_bound(graph, j);
            ScanPoint {
                executors: j,
                area_bound: area,
                lower_bound: critical_path.max(area),
                makespan: simulate(graph, j, policy).makespan(),
                graph_constrained: critical_path >= area,
                is_knee: j.get() == knee,
                direction: match measured_at {
                    None => Direction::Unknown,
                    Some(measured) if measured == j.get() => Direction::Measured,
                    Some(measured) if measured < j.get() => Direction::Optimistic,
                    Some(_) => Direction::Pessimistic,
                },
                measured_at,
            }
        })
        .collect()
}

fn executor_scenario(baseline: &Baseline, point: &ScanPoint) -> Scenario {
    let mut notes = Vec::new();
    if point.is_knee {
        notes.push(format!(
            "the knee: from {} executors on, the critical path dominates total work divided \
             between them, so more executors cannot shorten this build",
            point.executors
        ));
    }
    notes.push(match point.direction {
        Direction::Measured => {
            "the per-unit weights were measured at this executor count, so this is the one \
             absolute time in the scan that the model was calibrated against"
                .to_owned()
        }
        Direction::Optimistic => format!(
            "above the {} executors the weights were measured at, so this makespan is optimistic: \
             the same compilations take longer when more of them compete for memory bandwidth",
            point.measured_at.unwrap_or_default()
        ),
        Direction::Pessimistic => format!(
            "below the {} executors the weights were measured at, so this makespan is pessimistic: \
             the same compilations take less time when fewer of them run at once",
            point.measured_at.unwrap_or_default()
        ),
        Direction::Unknown => {
            "the timing report does not record what executor count the build ran \
             with, so how wrong this makespan is cannot be said"
                .to_owned()
        }
    });

    Scenario {
        kind: "set-executors",
        target: Target::Executors(point.executors.get()),
        // Nothing about the graph changes, so the graph after the change is known exactly. That is
        // a statement about the graph and not about the makespan, which the notes qualify.
        exact: true,
        fraction_swept: None,
        overhead: None,
        assumptions: vec![
            "the per-unit weights come from one build at one executor count and are not invariant \
             to it: measured elsewhere, total unit work fell 23% to 30% between ten executors and \
             two"
            .to_owned(),
            "one unit occupies one executor, which is not what Cargo does: a rustc holds one \
             jobserver token for its lifetime and acquires more for parallel codegen, and a \
             build-script execution takes none at all"
                .to_owned(),
        ],
        critical_path: Range {
            low: baseline.critical_path,
            high: baseline.critical_path,
        },
        // The critical path does not depend on the executor count, so changing it saves nothing
        // from the only quantity that holds under every scheduling policy.
        saving: Saving { low: 0, high: 0 },
        makespan: Range {
            low: point.makespan,
            high: point.makespan,
        },
        anomaly: false,
        notes,
    }
}

fn split_scenario(
    graph: &BuildGraph,
    baseline: &Baseline,
    id: UnitId,
    unit: &UnitFacts,
    noise: &Noise,
    policy: Policy,
) -> Option<Scenario> {
    let mut critical: Option<Range> = None;
    let mut makespan: Option<Range> = None;
    let mut anomaly = false;

    for (numerator, denominator) in SWEEP {
        let Some(denominator) = NonZeroU64::new(denominator) else {
            continue;
        };
        for dependents in [SplitDependents::Optimistic, SplitDependents::Pessimistic] {
            let transform = Transform::SplitUnit {
                unit: id,
                numerator,
                denominator,
                dependents,
                // Nobody has measured what a real split costs, so nothing is charged and the
                // output says so rather than inventing a number.
                overhead: Millis::ZERO,
            };
            let Ok(applied) = apply(graph, &transform) else {
                continue;
            };
            let path = analyze(applied.graph()).critical_path_len();
            let span = simulate(applied.graph(), baseline.executors, policy).makespan();
            widen(&mut critical, path);
            widen(&mut makespan, span);
            let path_moved = compare(baseline.critical_path, path);
            let span_moved = compare(baseline.makespan, span);
            if path_moved != 0 && span_moved != 0 && path_moved != span_moved {
                anomaly = true;
            }
        }
    }

    let critical = critical?;
    let makespan = makespan?;
    let saving = Saving {
        low: difference(baseline.critical_path, critical.high),
        high: difference(baseline.critical_path, critical.low),
    };

    let mut notes = Vec::new();
    if saving.high <= 0 {
        notes.push(
            "no division in the sweep shortens the critical path, so this crate is not worth \
             splitting for build time"
                .to_owned(),
        );
    } else if saving.low == saving.high {
        notes.push(
            "every division in the sweep gives the same critical path, because what the split \
             releases is bounded by something else; a range of zero width here is a degeneracy \
             rather than extra confidence in the number"
                .to_owned(),
        );
    }
    if let Some(spread) = noise.spread() {
        if saving.high < i64::try_from(spread.get()).unwrap_or(i64::MAX) {
            notes.push(format!(
                "below the noise floor: the most this could save is {}ms, against a run-to-run \
                 spread of {spread} measured across the supplied timing reports, so it is not \
                 distinguishable from noise",
                saving.high.max(0)
            ));
        }
    }
    if anomaly {
        notes.push(
            "somewhere in the sweep the simulated makespan moved opposite to the critical path, \
             which greedy list scheduling permits; the interval is anchored on the critical path, \
             which is monotone under this change"
                .to_owned(),
        );
    }

    Some(Scenario {
        kind: "split-crate",
        target: Target::Unit(unit.label.clone()),
        // Which half each dependent needs is not knowable without performing the split.
        exact: false,
        fraction_swept: Some(SWEEP_BOUNDS),
        overhead: Some(Millis::ZERO),
        assumptions: vec![
            format!(
                "the division of work between the halves is swept from 0.{:02} to 0.{:02}; no \
                 single division is assumed",
                SWEEP_BOUNDS.0, SWEEP_BOUNDS.1
            ),
            "each half pays the per-crate overhead given by overhead_ms, which defaults to zero \
             because no measurement of it exists"
                .to_owned(),
            "dependent assignment: optimistic and pessimistic bounds coincide at an even division, \
             so the interval is made of the sweep over the division rather than of that pair"
                .to_owned(),
            "the optimistic bound attaches every dependent to whichever half finishes first, \
             which is a limit a plausible refactor cannot beat rather than a shape anyone would \
             write"
                .to_owned(),
        ],
        critical_path: critical,
        saving,
        makespan,
        anomaly,
        notes,
    })
}

fn noise_diagnostics(noise: &Noise, scenarios: &[Scenario], reports: usize) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    match noise {
        Noise::Unknown => out.push(Diagnostic {
            severity: Severity::Warning,
            code: "noise-floor-unknown",
            message: format!(
                "only {} timing report was supplied, so the run-to-run spread could not be \
                 measured and no saving can be checked against it; supply --timings once per \
                 clean build to measure it",
                if reports == 1 { "one" } else { "no" }
            ),
        }),
        Noise::Measured { spread, samples } => {
            for scenario in scenarios {
                if scenario.kind != "split-crate" {
                    continue;
                }
                if scenario.saving.high >= i64::try_from(spread.get()).unwrap_or(i64::MAX) {
                    continue;
                }
                let Target::Unit(label) = &scenario.target else {
                    continue;
                };
                out.push(Diagnostic {
                    severity: Severity::Warning,
                    code: "saving-below-noise-floor",
                    message: format!(
                        "estimated saving of at most {}ms for {} is smaller than the {spread} \
                         run-to-run spread measured across the {} supplied timing reports",
                        scenario.saving.high.max(0),
                        label.display(),
                        samples.len()
                    ),
                });
            }
        }
    }
    out
}

/// Orders the counterfactuals as the machine-readable contract specifies: descending best saving,
/// then kind, then the target.
///
/// Two scenarios acting on the same executor count cannot both exist, and neither can two acting on
/// the same unit, so the order is total.
fn sort_scenarios(scenarios: &mut [Scenario]) {
    scenarios.sort_by(|a, b| {
        b.saving
            .high
            .cmp(&a.saving.high)
            .then(a.kind.cmp(b.kind))
            .then_with(|| target_key(&a.target).cmp(&target_key(&b.target)))
    });
}

/// A total order over targets: units by label, executor counts by number.
fn target_key(target: &Target) -> (Label, u32) {
    match target {
        Target::Unit(label) => (label.clone(), 0),
        Target::Executors(count) => (
            Label {
                package: String::new(),
                version: String::new(),
                target: String::new(),
            },
            *count,
        ),
    }
}

fn widen(range: &mut Option<Range>, value: Millis) {
    *range = Some(match *range {
        None => Range {
            low: value,
            high: value,
        },
        Some(current) => Range {
            low: current.low.min(value),
            high: current.high.max(value),
        },
    });
}

/// `before - after`, which may be negative because a change can lengthen a build.
fn difference(before: Millis, after: Millis) -> i64 {
    let before = i64::try_from(before.get()).unwrap_or(i64::MAX);
    let after = i64::try_from(after.get()).unwrap_or(i64::MAX);
    before.saturating_sub(after)
}

/// The sign of `before - after`.
fn compare(before: Millis, after: Millis) -> i32 {
    match before.cmp(&after) {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }
}

/// Two decimal places of `work / makespan`, as a count of hundredths.
///
/// Integer arithmetic throughout, so the printed value is identical on every platform. There is no
/// floating point anywhere in the output.
#[allow(clippy::integer_division)]
fn hundredths(work: Millis, makespan: Millis) -> u64 {
    let divisor = makespan.get();
    if divisor == 0 {
        return 0;
    }
    work.get()
        .saturating_mul(100)
        .saturating_add(divisor / 2)
        .saturating_div(divisor)
}
