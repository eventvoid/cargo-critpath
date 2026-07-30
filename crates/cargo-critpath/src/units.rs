//! Pairing each `rustc` invocation with the timing report's entry for the same unit.
//!
//! The two inputs describe the same units and share almost no identifiers. The verbose log knows a
//! unit by its crate name, its source path and its `-C extra-filename`; the timing report knows it
//! by package name, version, a display string and a feature list. Matching them is the one place
//! this adapter can be silently wrong, so it is done in stages of decreasing certainty, each stage
//! records which one succeeded, and anything left over is a refusal rather than a smaller graph.

use std::collections::BTreeMap;

use critpath_core::UnitKind;

use crate::error::{ParseError, UnitDescription};
use crate::metadata::PackageIndex;
use crate::timings::{TimingReport, TimingTarget, TimingUnit};
use crate::verbose::{Invocation, Role, VerboseLog};

/// How a unit's two halves were recognised as the same unit, in decreasing order of certainty.
///
/// Exposed because it is the honest answer to "how much do you trust this graph": a build whose
/// units were all matched by [`Manifest`](MatchMethod::Manifest) or
/// [`RegistryPath`](MatchMethod::RegistryPath) was identified exactly, and one that needed
/// [`StartOrder`](MatchMethod::StartOrder) was not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum MatchMethod {
    /// The source path lay under a package's manifest directory, per `cargo metadata`. Exact.
    Manifest,
    /// The source path contained `name-version/`, as a registry or git checkout does. Exact.
    RegistryPath,
    /// The unit is a build script, and Cargo gives every build script a
    /// `target/<profile>/build/<package>-<hash>` directory. Exact.
    BuildDirectory,
    /// The `--crate-name` matched a package or target name. Right unless a target was renamed.
    CrateName,
    /// The crate's own directory name matched a package name. A heuristic; a package need not be
    /// named after its directory.
    Directory,
    /// Several candidates shared a name and version, and exactly one had the same feature set.
    Features,
    /// Several candidates were indistinguishable, so they were paired in the order each side lists
    /// them. They differ only in weight, since everything else about them is identical.
    StartOrder,
    /// Exactly one unit of this kind was left unmatched on each side.
    SoleCandidate,
}

impl MatchMethod {
    /// A short stable name, for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::RegistryPath => "registry-path",
            Self::BuildDirectory => "build-directory",
            Self::CrateName => "crate-name",
            Self::Directory => "directory",
            Self::Features => "features",
            Self::StartOrder => "start-order",
            Self::SoleCandidate => "sole-candidate",
        }
    }

    /// Whether this method identified the unit rather than deduced it.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        matches!(
            self,
            Self::Manifest | Self::RegistryPath | Self::BuildDirectory
        )
    }
}

/// One invocation paired with one timing entry.
pub(crate) struct Pairing {
    pub(crate) invocation: usize,
    pub(crate) unit: usize,
    pub(crate) method: MatchMethod,
}

/// The kinds of unit that can only ever match each other.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    /// A library of some sort: the timing report cannot say which.
    Library,
    BuildScriptCompile,
    BuildScriptRun,
    /// A named target, keyed on the kind Cargo printed, such as `bin`.
    Named(String),
}

/// Pairs every invocation with its timing entry, or says exactly which ones it could not.
pub(crate) fn pair(
    log: &VerboseLog,
    report: &TimingReport,
    packages: Option<&PackageIndex>,
) -> Result<Vec<Pairing>, ParseError> {
    let mut index: BTreeMap<(Group, String), Vec<usize>> = BTreeMap::new();
    for (position, unit) in report.units.iter().enumerate() {
        let group = group_of_unit(&unit.target);
        index
            .entry((group.clone(), normalise(&unit.package)))
            .or_default()
            .push(position);
        if let TimingTarget::Named { name, .. } = &unit.target {
            let key = (group, normalise(name));
            let slot = index.entry(key).or_default();
            if !slot.contains(&position) {
                slot.push(position);
            }
        }
    }

    // A build-script execution has no source path and no crate name of its own, but the directory
    // it runs from carries the compiled script's `-C extra-filename`. Resolving that to the
    // compilation gives the execution the same package, version and features, which is what
    // separates two versions of one package that both have a build script.
    let by_artifact: BTreeMap<&str, &Invocation> = log
        .invocations
        .iter()
        .filter(|i| i.role == Role::Compile)
        .filter_map(|i| Some((i.extra_filename.as_deref()?, i)))
        .collect();

    let mut claimed = vec![false; report.units.len()];
    let mut pairings: Vec<Option<Pairing>> = Vec::with_capacity(log.invocations.len());

    for (position, invocation) in log.invocations.iter().enumerate() {
        let group = group_of_invocation(invocation);
        let identity = identity_of(invocation, &by_artifact);
        let mut found = None;
        for (method, name, version) in candidates(invocation, identity, packages) {
            let Some(pool) = index.get(&(group.clone(), normalise(&name))) else {
                continue;
            };
            let available: Vec<usize> = pool
                .iter()
                .copied()
                .filter(|&i| claimed.get(i) == Some(&false))
                .collect();
            if let Some((unit, refined)) =
                choose(&available, &report.units, identity, version.as_deref())
            {
                found = Some(Pairing {
                    invocation: position,
                    unit,
                    method: refined.unwrap_or(method),
                });
                break;
            }
        }
        if let Some(pairing) = &found {
            if let Some(slot) = claimed.get_mut(pairing.unit) {
                *slot = true;
            }
        }
        pairings.push(found);
    }

    // Anything still unmatched: if exactly one invocation and one unit of a kind are left, they are
    // each other. This is what recovers a target whose name matches nothing, such as a binary in a
    // package that renamed it.
    resolve_sole_candidates(log, report, &mut pairings, &mut claimed);

    let in_log: Vec<UnitDescription> = log
        .invocations
        .iter()
        .zip(pairings.iter())
        .filter(|(_, pairing)| pairing.is_none())
        .map(|(invocation, _)| {
            describe_invocation(invocation, identity_of(invocation, &by_artifact), packages)
        })
        .collect();
    let in_report: Vec<UnitDescription> = report
        .units
        .iter()
        .enumerate()
        .filter(|(i, _)| claimed.get(*i) != Some(&true))
        .map(|(_, unit)| describe_unit(unit))
        .collect();

    if !in_log.is_empty() || !in_report.is_empty() {
        return Err(ParseError::UnmatchedUnits {
            in_log,
            in_report,
            had_metadata: packages.is_some(),
        });
    }

    Ok(pairings.into_iter().flatten().collect())
}

/// Picks one of several candidates.
///
/// Returns the chosen unit, and the method that separated it when the name alone did not.
fn choose(
    available: &[usize],
    units: &[TimingUnit],
    invocation: &Invocation,
    version: Option<&str>,
) -> Option<(usize, Option<MatchMethod>)> {
    if available.is_empty() {
        return None;
    }
    // A version from the source path or the manifest is part of the identity: a candidate with a
    // different version is a different package, not a worse match.
    let narrowed: Vec<usize> = match version {
        Some(version) => {
            let same: Vec<usize> = available
                .iter()
                .copied()
                .filter(|&i| units.get(i).is_some_and(|u| u.version == version))
                .collect();
            if same.is_empty() {
                return None;
            }
            same
        }
        None => available.to_vec(),
    };

    if let [only] = narrowed.as_slice() {
        return Some((*only, None));
    }

    // Cargo can build one package twice at one version, for the host and for the target, and the
    // two instances usually differ in their resolved features.
    let same_features: Vec<usize> = narrowed
        .iter()
        .copied()
        .filter(|&i| {
            units
                .get(i)
                .is_some_and(|u| u.features == invocation.features)
        })
        .collect();
    if let [only] = same_features.as_slice() {
        return Some((*only, Some(MatchMethod::Features)));
    }

    // Otherwise the candidates are indistinguishable: same package, same version, same features.
    // Both sides list units in start order, so the first unclaimed one is the best available
    // answer, and the method records that it was not identified.
    narrowed
        .first()
        .map(|&i| (i, Some(MatchMethod::StartOrder)))
}

fn resolve_sole_candidates(
    log: &VerboseLog,
    report: &TimingReport,
    pairings: &mut [Option<Pairing>],
    claimed: &mut [bool],
) {
    let outstanding: Vec<usize> = pairings
        .iter()
        .enumerate()
        .filter(|(_, pairing)| pairing.is_none())
        .map(|(i, _)| i)
        .collect();
    if outstanding.is_empty() {
        return;
    }

    let mut by_group: BTreeMap<Group, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for position in outstanding {
        if let Some(invocation) = log.invocations.get(position) {
            by_group
                .entry(group_of_invocation(invocation))
                .or_default()
                .0
                .push(position);
        }
    }
    for (position, unit) in report.units.iter().enumerate() {
        if claimed.get(position) == Some(&true) {
            continue;
        }
        by_group
            .entry(group_of_unit(&unit.target))
            .or_default()
            .1
            .push(position);
    }

    for (invocations, units) in by_group.into_values() {
        let ([invocation], [unit]) = (invocations.as_slice(), units.as_slice()) else {
            continue;
        };
        if let Some(slot) = pairings.get_mut(*invocation) {
            *slot = Some(Pairing {
                invocation: *invocation,
                unit: *unit,
                method: MatchMethod::SoleCandidate,
            });
        }
        if let Some(slot) = claimed.get_mut(*unit) {
            *slot = true;
        }
    }
}

/// Every name this invocation might be known by, most certain first.
fn candidates(
    invocation: &Invocation,
    identity: &Invocation,
    packages: Option<&PackageIndex>,
) -> Vec<(MatchMethod, String, Option<String>)> {
    let mut out: Vec<(MatchMethod, String, Option<String>)> = Vec::new();
    let mut push = |method, name: &str, version: Option<String>| {
        if name.is_empty() {
            return;
        }
        out.push((method, name.to_owned(), version));
    };

    if let Some((name, version)) = manifest_package(identity, packages) {
        push(MatchMethod::Manifest, &name, Some(version));
    }
    if let Some(package) = &identity.registry_package {
        push(
            MatchMethod::RegistryPath,
            &package.name,
            Some(package.version.clone()),
        );
    }
    if let Some(package) = &invocation.run_package {
        push(MatchMethod::BuildDirectory, package, None);
    }
    push(MatchMethod::CrateName, &invocation.crate_name, None);
    if let Some(package) = &identity.directory_package {
        push(MatchMethod::Directory, package, None);
    }

    out.dedup_by(|a, b| a.1 == b.1 && a.2 == b.2);
    out
}

fn group_of_unit(target: &TimingTarget) -> Group {
    match target {
        TimingTarget::Library => Group::Library,
        TimingTarget::BuildScriptCompile => Group::BuildScriptCompile,
        TimingTarget::BuildScriptRun => Group::BuildScriptRun,
        TimingTarget::Named { kind, .. } => Group::Named(kind.clone()),
        TimingTarget::Other(text) => Group::Named(text.clone()),
    }
}

fn group_of_invocation(invocation: &Invocation) -> Group {
    if invocation.role == Role::BuildScriptRun {
        return Group::BuildScriptRun;
    }
    match invocation.kind {
        UnitKind::BuildScriptCompile => Group::BuildScriptCompile,
        UnitKind::Bin => Group::Named("bin".to_owned()),
        UnitKind::Test => Group::Named("test".to_owned()),
        // A procedural macro renders as a library in the timing report, which is exactly why the
        // verbose log is needed to tell them apart.
        _ => Group::Library,
    }
}

/// Package names use dashes and crate names use underscores for the same package.
fn normalise(name: &str) -> String {
    name.replace('-', "_")
}

/// The invocation whose identity applies: a build-script execution borrows its compilation's.
fn identity_of<'a>(
    invocation: &'a Invocation,
    by_artifact: &BTreeMap<&str, &'a Invocation>,
) -> &'a Invocation {
    if invocation.role != Role::BuildScriptRun {
        return invocation;
    }
    invocation
        .run_artifact
        .as_deref()
        .and_then(|hash| by_artifact.get(hash).copied())
        .unwrap_or(invocation)
}

/// The package a `cargo metadata` index attributes this invocation's source path to.
fn manifest_package(
    invocation: &Invocation,
    packages: Option<&PackageIndex>,
) -> Option<(String, String)> {
    if invocation.source_path.is_empty() {
        return None;
    }
    packages?
        .package_at(&invocation.source_path)
        .map(|(name, version)| (name.to_owned(), version.to_owned()))
}

fn describe_invocation(
    invocation: &Invocation,
    identity: &Invocation,
    packages: Option<&PackageIndex>,
) -> UnitDescription {
    let (package, version) = manifest_package(identity, packages)
        .or_else(|| {
            identity
                .registry_package
                .as_ref()
                .map(|p| (p.name.clone(), p.version.clone()))
        })
        .or_else(|| {
            invocation
                .run_package
                .clone()
                .map(|name| (name, String::new()))
        })
        .unwrap_or_else(|| (invocation.crate_name.clone(), String::new()));
    UnitDescription {
        package,
        version,
        target: invocation.kind.as_str().to_owned(),
        position: invocation.position.saturating_add(1),
    }
}

fn describe_unit(unit: &TimingUnit) -> UnitDescription {
    UnitDescription {
        package: unit.package.clone(),
        version: unit.version.clone(),
        target: unit.target.label().to_owned(),
        position: unit.position.saturating_add(1),
    }
}
