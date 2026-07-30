//! Assembling the matched halves into a graph, and everything a report needs to say where it came
//! from.

use std::collections::{BTreeMap, BTreeSet};

use critpath_core::{BuildGraph, EdgeKind, GraphBuilder, Millis, Unit, UnitId, UnitKind};

use crate::error::{ParseError, UnitDescription};
use crate::metadata::PackageIndex;
use crate::timings::{TimingReport, TimingUnit};
use crate::units::{self, MatchMethod};
use crate::verbose::{Invocation, Role, VerboseLog};

/// How serious a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Something the reader should know, which did not stop the analysis.
    Warning,
}

/// Something worth telling the reader about the input, which was not bad enough to refuse.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct Diagnostic {
    /// How serious it is.
    pub severity: Severity,
    /// A stable kebab-case identifier, which is the field to match on.
    pub code: &'static str,
    /// Prose, which may change in any release.
    pub message: String,
}

/// Which of the report's `unblocked_*` fields a pair came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnblockedSource {
    /// The producer finished.
    Completion,
    /// The producer emitted its metadata.
    Metadata,
}

/// One pair the timing report claims a dependency between.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct UnblockedPair {
    /// The unit that unblocked the other.
    pub producer: UnitId,
    /// The unit that was unblocked.
    pub consumer: UnitId,
    /// What the producer had to do for it.
    pub source: UnblockedSource,
}

/// The result of checking the report's `unblocked_*` fields against the graph the log gave.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnblockedCheck {
    /// How many pairs the report recorded.
    pub total: usize,
    /// How many of them the graph agrees are dependencies.
    pub confirmed: usize,
    /// The ones it does not, which is a defect in the parser rather than in Cargo.
    pub unconfirmed: Vec<UnblockedPair>,
}

impl UnblockedCheck {
    /// Whether every pair the report recorded is a dependency the graph has.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.unconfirmed.is_empty()
    }
}

/// Where a build's numbers came from.
///
/// Every field the timing report does not state is `None` rather than guessed. `cargo_version` is
/// one of those: the report's header names the `rustc` version and not Cargo's, so a caller that
/// knows it should fill this in.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BuildMeta {
    /// The Cargo version, which the timing report does not state.
    pub cargo_version: Option<String>,
    /// The `rustc` version, from the timing report's header.
    pub rustc_version: Option<String>,
    /// The host triple.
    pub host: Option<String>,
    /// The target triple.
    pub target: Option<String>,
    /// The profile the build used.
    pub profile: Option<String>,
    /// The `-j` the build ran with.
    pub jobs: Option<u32>,
    /// The machine's processor count.
    pub cpus: Option<u32>,
    /// The peak number of units Cargo actually had in flight, which can exceed `jobs` because a
    /// build-script execution takes no compile token.
    pub observed_max_concurrency: Option<u32>,
    /// Units in the build.
    pub units_total: usize,
    /// Of those, units that were reused from the cache. Always zero, since a build with any is
    /// refused.
    pub units_fresh: usize,
    /// The last unit's end minus the first unit's start.
    pub compilation_span: Millis,
    /// Cargo's own total for the build.
    pub reported_total: Option<Millis>,
    /// The reported total minus the compilation span: fetching, resolution, and anything else
    /// outside the unit model. Reported rather than folded in.
    pub excluded_prefix: Millis,
    /// The targets the build was asked for, as the report's header displays them.
    pub targets: Option<String>,
    /// When the build started, exactly as the report wrote it.
    pub build_start: Option<String>,
}

/// One build, ready to analyse.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BuildInput {
    /// The graph the two inputs agreed on.
    pub graph: BuildGraph,
    /// Where its numbers came from.
    pub build: BuildMeta,
    /// How each unit's two halves were recognised as the same unit, by unit identifier. A build
    /// whose entries are all exact was identified rather than deduced.
    pub matched_by: Vec<MatchMethod>,
    /// The pairs the timing report claims are dependencies, for [`check_unblocked`].
    ///
    /// [`check_unblocked`]: BuildInput::check_unblocked
    pub unblocked: Vec<UnblockedPair>,
    /// Anything worth telling the reader that did not stop the analysis.
    pub diagnostics: Vec<Diagnostic>,
}

impl BuildInput {
    /// Checks the timing report's `unblocked_*` fields against the graph the verbose log gave.
    ///
    /// The fields are not the dependency graph — they record only the unit that happened to be each
    /// unit's *last* blocker, which covered 51% of the real edges on a measured 28-unit build — but
    /// every pair in them must still be a dependency. That makes them an independent check on the
    /// matcher and the edge rules, from data the parser did not use to build the graph.
    ///
    /// A pair is confirmed when the graph has the edge, or when the consumer requires its whole
    /// closure and the producer is somewhere in it. The second case is not a concession: Cargo's own
    /// job queue materialises exactly those closure edges for a unit that needs upstream object
    /// files, and this model expresses them as a property of the consumer instead.
    #[must_use]
    pub fn check_unblocked(&self) -> UnblockedCheck {
        let mut pairs = self.unblocked.clone();
        // Grouped by consumer so that each closure is computed once.
        pairs.sort();

        let mut confirmed = 0usize;
        let mut unconfirmed = Vec::new();
        let mut closure_of: Option<(UnitId, BTreeSet<UnitId>)> = None;

        for pair in &pairs {
            let direct = self
                .graph
                .dependencies(pair.consumer)
                .is_some_and(|deps| deps.iter().any(|d| d.unit == pair.producer));
            if direct {
                confirmed = confirmed.saturating_add(1);
                continue;
            }
            let needs_closure = self
                .graph
                .unit(pair.consumer)
                .is_some_and(|u| u.requires_full_closure);
            if needs_closure {
                let cached = match &closure_of {
                    Some((consumer, set)) if *consumer == pair.consumer => set,
                    _ => {
                        closure_of = Some((pair.consumer, self.closure(pair.consumer)));
                        match &closure_of {
                            Some((_, set)) => set,
                            None => continue,
                        }
                    }
                };
                if cached.contains(&pair.producer) {
                    confirmed = confirmed.saturating_add(1);
                    continue;
                }
            }
            unconfirmed.push(*pair);
        }

        UnblockedCheck {
            total: pairs.len(),
            confirmed,
            unconfirmed,
        }
    }

    /// Every unit the given one transitively depends on.
    fn closure(&self, unit: UnitId) -> BTreeSet<UnitId> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<UnitId> = self
            .graph
            .dependencies(unit)
            .unwrap_or_default()
            .iter()
            .map(|d| d.unit)
            .collect();
        while let Some(next) = stack.pop() {
            if !seen.insert(next) {
                continue;
            }
            for dep in self.graph.dependencies(next).unwrap_or_default() {
                stack.push(dep.unit);
            }
        }
        seen
    }
}

/// Everything a matched unit needs before it becomes a node.
struct Assembled<'a> {
    invocation: &'a Invocation,
    timing: &'a TimingUnit,
    method: MatchMethod,
}

/// Builds the graph from a matched verbose log and timing report.
pub(crate) fn assemble(
    log: &VerboseLog,
    report: &TimingReport,
    packages: Option<&PackageIndex>,
) -> Result<BuildInput, ParseError> {
    if report.units.is_empty() {
        return Err(ParseError::EmptyBuild);
    }
    let pairings = units::pair(log, report, packages)?;

    let mut assembled: Vec<Assembled<'_>> = Vec::with_capacity(pairings.len());
    for pairing in &pairings {
        let (Some(invocation), Some(timing)) = (
            log.invocations.get(pairing.invocation),
            report.units.get(pairing.unit),
        ) else {
            continue;
        };
        assembled.push(Assembled {
            invocation,
            timing,
            method: pairing.method,
        });
    }

    // Invariant I1: the graph must be reproducible, so units enter it in an order derived from
    // their identity rather than from the order Cargo happened to start them. The compilation hash
    // is the tie-break of last resort, because two instances of one package can agree on everything
    // else.
    assembled.sort_by(|a, b| {
        a.timing
            .package
            .cmp(&b.timing.package)
            .then_with(|| a.timing.version.cmp(&b.timing.version))
            .then_with(|| a.timing.target.label().cmp(b.timing.target.label()))
            .then_with(|| a.invocation.kind.as_str().cmp(b.invocation.kind.as_str()))
            .then_with(|| a.invocation.metadata_hash.cmp(&b.invocation.metadata_hash))
            .then_with(|| a.timing.index.cmp(&b.timing.index))
    });

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut builder = GraphBuilder::with_capacity(assembled.len());
    let mut matched_by = Vec::with_capacity(assembled.len());
    let mut ids = Vec::with_capacity(assembled.len());

    for entry in &assembled {
        // Sections appear for pipelined units, but it is the compiler invocation that decides
        // whether anything may start against this unit early, so the log has the last word.
        let metadata_at = if entry.invocation.pipelined {
            entry.timing.metadata_at
        } else {
            None
        };
        let id = builder.add_unit(Unit {
            package: entry.timing.package.clone(),
            version: entry.timing.version.clone(),
            target: entry.timing.target.label().to_owned(),
            kind: entry.invocation.kind,
            total: entry.timing.duration,
            metadata_at,
            requires_full_closure: entry.invocation.requires_full_closure,
        })?;
        ids.push(id);
        matched_by.push(entry.method);
    }

    add_dependency_edges(&assembled, &ids, &mut builder, &mut diagnostics)?;
    add_build_script_edges(&assembled, &ids, &mut builder, &mut diagnostics)?;

    let graph = builder.build()?;
    let unblocked = unblocked_pairs(&assembled, &ids);
    report_inexact_matches(&matched_by, &mut diagnostics);

    Ok(BuildInput {
        graph,
        build: meta(report),
        matched_by,
        unblocked,
        diagnostics,
    })
}

/// Edges named by `--extern`, which is every dependency Cargo passes to the compiler.
fn add_dependency_edges(
    assembled: &[Assembled<'_>],
    ids: &[UnitId],
    builder: &mut GraphBuilder,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), ParseError> {
    let mut by_artifact: BTreeMap<&str, UnitId> = BTreeMap::new();
    for (entry, id) in assembled.iter().zip(ids.iter()) {
        if let Some(hash) = &entry.invocation.extra_filename {
            by_artifact.insert(hash.as_str(), *id);
        }
    }

    for (entry, consumer) in assembled.iter().zip(ids.iter()) {
        for dependency in &entry.invocation.externs {
            let Some(hash) = &dependency.producer else {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    code: "extern-without-artifact-hash",
                    message: format!(
                        "{} names `{}` through an artifact with no compilation hash, so it is not \
                         one of this build's units and no edge was added for it",
                        label(entry),
                        dependency.name
                    ),
                });
                continue;
            };
            let Some(producer) = by_artifact.get(hash.as_str()).copied() else {
                return Err(ParseError::UnresolvedDependency {
                    consumer: Box::new(describe(entry)),
                    dependency: dependency.name.clone(),
                    producer: hash.clone(),
                });
            };
            let kind = downgrade(
                dependency.edge,
                producer,
                assembled,
                ids,
                entry,
                diagnostics,
            );
            builder.add_edge(producer, *consumer, kind)?;
        }
    }
    Ok(())
}

/// A metadata edge whose producer emits no metadata cannot be honoured, and the core rejects it.
/// Treating it as a full edge is the conservative reading; treating it as satisfied immediately
/// would be optimistic, which is the direction that must never be taken silently.
fn downgrade(
    kind: EdgeKind,
    producer: UnitId,
    assembled: &[Assembled<'_>],
    ids: &[UnitId],
    consumer: &Assembled<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> EdgeKind {
    if kind != EdgeKind::Metadata {
        return kind;
    }
    let emits = ids
        .iter()
        .position(|id| *id == producer)
        .and_then(|at| assembled.get(at))
        .is_some_and(|entry| entry.invocation.pipelined && entry.timing.metadata_at.is_some());
    if emits {
        return EdgeKind::Metadata;
    }
    diagnostics.push(Diagnostic {
        severity: Severity::Warning,
        code: "metadata-edge-without-metadata",
        message: format!(
            "{} was given a dependency as metadata, but the timing report records no metadata \
             moment for it, so the edge was treated as needing the whole dependency; \
             the critical path this produces is an upper bound rather than an estimate",
            label(consumer)
        ),
    });
    EdgeKind::Full
}

/// Cargo's three-unit build-script pattern, which no `--extern` names: compiling the script, running
/// it, and the package's own units waiting on the run.
fn add_build_script_edges(
    assembled: &[Assembled<'_>],
    ids: &[UnitId],
    builder: &mut GraphBuilder,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), ParseError> {
    let mut by_artifact: BTreeMap<&str, UnitId> = BTreeMap::new();
    for (entry, id) in assembled.iter().zip(ids.iter()) {
        if let Some(hash) = &entry.invocation.extra_filename {
            by_artifact.insert(hash.as_str(), *id);
        }
    }

    for (entry, run) in assembled.iter().zip(ids.iter()) {
        if entry.invocation.role != Role::BuildScriptRun {
            continue;
        }
        // The run executes out of `build/<package>-<hash>`, and that hash is the compiled script's
        // own `-C extra-filename`, so the compile-to-run edge is stated by the log rather than
        // guessed from a name.
        let Some(hash) = &entry.invocation.run_artifact else {
            return Err(ParseError::UnresolvedDependency {
                consumer: Box::new(describe(entry)),
                dependency: "its compiled build script".to_owned(),
                producer: "<no compilation hash in the execution path>".to_owned(),
            });
        };
        let Some(compile) = by_artifact.get(hash.as_str()).copied() else {
            return Err(ParseError::UnresolvedDependency {
                consumer: Box::new(describe(entry)),
                dependency: "its compiled build script".to_owned(),
                producer: hash.clone(),
            });
        };
        builder.add_edge(compile, *run, EdgeKind::Full)?;

        // Everything else Cargo built for the same package instance waits for the run. Features
        // separate two instances of one package at one version; where they do not, the fallback
        // widens to the whole package and says so, because a missing edge understates the critical
        // path and a spurious one does not.
        let mut dependents = package_units(assembled, ids, entry, true);
        if dependents.is_empty() {
            dependents = package_units(assembled, ids, entry, false);
            if !dependents.is_empty() {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    code: "build-script-instance-ambiguous",
                    message: format!(
                        "the build script of {} shares no feature set with any of that package's \
                         compilations, so every one of them was made to wait for it; \
                         if the package is built twice with different features this is stricter \
                         than the real build",
                        label(entry)
                    ),
                });
            }
        }
        for dependent in dependents {
            builder.add_edge(*run, dependent, EdgeKind::Full)?;
        }
    }
    Ok(())
}

/// The compilations belonging to the same package instance as a build-script execution.
fn package_units(
    assembled: &[Assembled<'_>],
    ids: &[UnitId],
    run: &Assembled<'_>,
    match_features: bool,
) -> Vec<UnitId> {
    assembled
        .iter()
        .zip(ids.iter())
        .filter(|(entry, _)| {
            entry.invocation.kind != UnitKind::BuildScriptCompile
                && entry.invocation.kind != UnitKind::BuildScriptRun
                && entry.timing.package == run.timing.package
                && entry.timing.version == run.timing.version
                && (!match_features || entry.timing.features == run.timing.features)
        })
        .map(|(_, id)| *id)
        .collect()
}

/// The report's `unblocked_*` fields, translated into unit identifiers.
fn unblocked_pairs(assembled: &[Assembled<'_>], ids: &[UnitId]) -> Vec<UnblockedPair> {
    let mut by_index: BTreeMap<usize, UnitId> = BTreeMap::new();
    for (entry, id) in assembled.iter().zip(ids.iter()) {
        by_index.insert(entry.timing.index, *id);
    }

    let mut pairs = Vec::new();
    for (entry, producer) in assembled.iter().zip(ids.iter()) {
        for (indices, source) in [
            (&entry.timing.unblocked, UnblockedSource::Completion),
            (&entry.timing.unblocked_rmeta, UnblockedSource::Metadata),
        ] {
            for index in indices.iter() {
                if let Some(consumer) = by_index.get(index).copied() {
                    pairs.push(UnblockedPair {
                        producer: *producer,
                        consumer,
                        source,
                    });
                }
            }
        }
    }
    pairs.sort();
    pairs.dedup();
    pairs
}

fn report_inexact_matches(methods: &[MatchMethod], diagnostics: &mut Vec<Diagnostic>) {
    let ordered = methods.iter().filter(|m| **m == MatchMethod::StartOrder);
    let count = ordered.count();
    if count > 0 {
        diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            code: "units-matched-by-order",
            message: format!(
                "{count} unit(s) could not be told apart from another unit of the same package, \
                 version and feature set, so each was paired with a timing entry by start order; \
                 they differ only in weight, and swapping two of them would move work between two \
                 identical compilations"
            ),
        });
    }
}

fn meta(report: &TimingReport) -> BuildMeta {
    BuildMeta {
        cargo_version: None,
        rustc_version: report.rustc_version.clone(),
        host: report.host.clone(),
        target: report.target.clone(),
        profile: report.profile.clone(),
        jobs: report.jobs,
        cpus: report.cpus,
        observed_max_concurrency: report.max_concurrency,
        units_total: report.total_units,
        units_fresh: report.fresh_units,
        compilation_span: report.compilation_span(),
        reported_total: report.reported_total,
        excluded_prefix: report.excluded_prefix(),
        targets: report.targets.clone(),
        build_start: report.build_start.clone(),
    }
}

fn label(entry: &Assembled<'_>) -> String {
    format!(
        "{} {} ({})",
        entry.timing.package,
        entry.timing.version,
        entry.timing.target.label()
    )
}

fn describe(entry: &Assembled<'_>) -> UnitDescription {
    UnitDescription {
        package: entry.timing.package.clone(),
        version: entry.timing.version.clone(),
        target: entry.timing.target.label().to_owned(),
        position: entry.timing.position.saturating_add(1),
    }
}
