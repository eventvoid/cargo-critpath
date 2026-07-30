//! Properties the model promises, checked over a corpus of arbitrary build graphs.
//!
//! Every test here encodes an invariant of `docs/model.md` section 7, quoted above the test, plus
//! the transformation properties of section 9. The expectations are derived from that document
//! rather than from the code: where a shape has a closed form the test computes it by hand, and
//! where a property is universal the test replays the produced answer against the definitions of
//! sections 3 to 6.
//!
//! The corpus comes from a hand-rolled xorshift generator seeded from a fixed constant, so a
//! failure names a graph index that reproduces exactly on the next run and on any other machine.
//! `corpus_is_non_trivial` states what the generator is supposed to produce and fails if it stops
//! producing it, because a property test over a corpus that lacks the interesting case is a lie.

// A test reports failure by panicking, so unwrapping a value the surrounding assertion has just
// established is present is clearer than propagating an error no caller can act on, and a
// deliberate panic is how a match arm reports an outcome no assertion can phrase. Indexing is into
// vectors this file allocated at a known length. The divisions recompute the area bound of section
// 6 in long form, so that the expected value does not come from the helper under test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division
)]

use std::collections::BTreeSet;
use std::num::{NonZeroU32, NonZeroU64};

use critpath_core::{
    analyze, apply, area_bound, lower_bound, roots, simulate, Analysis, BuildGraph, EdgeKind,
    GraphBuilder, Mapped, Millis, Policy, SplitDependents, Transform, Unit, UnitId, UnitKind,
};

// ---------------------------------------------------------------------------------------------
// A deterministic source of arbitrary graphs
// ---------------------------------------------------------------------------------------------

/// The seed every corpus in this file starts from. Fixed so that a failure reproduces exactly.
const CORPUS_SEED: u64 = 0x5EED_C471_9A3B_0001;

/// How many graphs the shared corpus holds.
///
/// Sized so that the whole file runs in a couple of seconds in a debug build: the heaviest tests
/// simulate the corpus at five executor counts under both policies.
const CORPUS_SIZE: usize = 160;

/// Executor counts every bounded-schedule property is checked at.
const EXECUTORS: [u32; 5] = [1, 2, 3, 5, 8];

/// Both scheduling policies of section 5.
const POLICIES: [Policy; 2] = [Policy::CargoLike, Policy::LongestPath];

/// A duration no real graph can reach, used as the identity of a minimum.
const INF: Millis = Millis::new(u64::MAX);

/// xorshift64*, so that the corpus is reproducible without taking on a dependency.
struct Rng(u64);

impl Rng {
    /// A generator seeded from `seed`, which must not be zero.
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n`, or zero if `n` is zero.
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }

    /// A value in `lo..=hi`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo.saturating_add(self.below(hi.saturating_sub(lo).saturating_add(1)))
    }

    /// True with probability `numerator / denominator`.
    fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.below(denominator) < numerator
    }
}

/// Which family of weights a graph draws from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Weights {
    /// Every unit takes no time at all.
    Zero,
    /// Every unit is identical, which is where tie-breaking bugs hide.
    Equal,
    /// Ordinary durations, including the occasional zero.
    Mixed,
    /// Durations large enough that sums are far outside anything a build produces.
    Huge,
    /// Durations close enough to the representable maximum that sums saturate.
    Extreme,
}

/// The shape of a graph's dependency skeleton.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// One unit after another.
    Chain,
    /// One source releasing everything else.
    Fan,
    /// Repeated four-unit diamonds, sometimes joined end to end.
    Diamond,
    /// Layers of a few units each, every unit depending on some of the previous layer.
    Layered,
    /// Random sparse edges over the whole index range.
    Sparse,
    /// Several sparse blocks with no edge between them.
    Disconnected,
}

impl Shape {
    /// Every shape the generator can produce, so that the corpus can be shown to contain each.
    const ALL: [Self; 6] = [
        Self::Chain,
        Self::Fan,
        Self::Diamond,
        Self::Layered,
        Self::Sparse,
        Self::Disconnected,
    ];

    fn index(self) -> usize {
        match self {
            Self::Chain => 0,
            Self::Fan => 1,
            Self::Diamond => 2,
            Self::Layered => 3,
            Self::Sparse => 4,
            Self::Disconnected => 5,
        }
    }
}

/// A unit before it is given the names the graph type wants.
#[derive(Clone, Debug)]
struct UnitSpec {
    total: u64,
    metadata_at: Option<u64>,
    closure: bool,
    kind: UnitKind,
}

/// A graph description that can be rebuilt after being edited, which is how the counterfactual
/// invariants construct the graph they compare against.
#[derive(Clone, Debug)]
struct GraphSpec {
    units: Vec<UnitSpec>,
    /// Producer index, consumer index, and what the consumer needs. Producer index is always the
    /// smaller of the two, which is what makes the result acyclic by construction.
    edges: Vec<(usize, usize, EdgeKind)>,
    shape: Shape,
    weights: Weights,
}

impl GraphSpec {
    fn build(&self) -> BuildGraph {
        let mut builder = GraphBuilder::with_capacity(self.units.len());
        let mut ids = Vec::with_capacity(self.units.len());
        for (i, spec) in self.units.iter().enumerate() {
            let unit = Unit {
                package: format!("p{i}"),
                version: "0.1.0".to_owned(),
                target: format!("t{i}"),
                kind: spec.kind,
                total: Millis::new(spec.total),
                metadata_at: spec.metadata_at.map(Millis::new),
                requires_full_closure: spec.closure,
            };
            ids.push(builder.add_unit(unit).unwrap());
        }
        for &(producer, consumer, kind) in &self.edges {
            builder
                .add_edge(ids[producer], ids[consumer], kind)
                .unwrap();
        }
        builder.build().unwrap()
    }

    fn critical_path_len(&self) -> Millis {
        analyze(&self.build()).critical_path_len()
    }
}

fn random_kind(rng: &mut Rng) -> UnitKind {
    match rng.below(100) {
        0..=59 => UnitKind::Lib,
        60..=69 => UnitKind::Bin,
        70..=77 => UnitKind::Test,
        78..=85 => UnitKind::ProcMacro,
        86..=90 => UnitKind::BuildScriptCompile,
        91..=95 => UnitKind::BuildScriptRun,
        _ => UnitKind::Other,
    }
}

fn random_unit(rng: &mut Rng, weights: Weights) -> UnitSpec {
    if weights == Weights::Equal {
        // Deliberately identical: equal weights and equal metadata offsets maximise the number of
        // ready units a scheduler must choose between at the same instant.
        return UnitSpec {
            total: 100,
            metadata_at: Some(60),
            closure: false,
            kind: UnitKind::Lib,
        };
    }

    let total = match weights {
        Weights::Zero => 0,
        Weights::Equal => 100,
        Weights::Mixed => rng.below(5_001),
        Weights::Huge => rng.range(1_000_000_000_000, 2_000_000_000_000),
        Weights::Extreme => rng.range(u64::MAX / 4, u64::MAX - 1),
    };

    // Two thirds pipelined, and among those the boundary sits at zero, at the very end, and
    // somewhere in between in roughly equal measure.
    let metadata_at = if rng.chance(2, 3) {
        Some(match rng.below(4) {
            0 => 0,
            1 => total,
            _ => rng.below(total.saturating_add(1)),
        })
    } else {
        None
    };

    UnitSpec {
        total,
        metadata_at,
        closure: rng.chance(1, 4),
        kind: random_kind(rng),
    }
}

fn sparse_edges(lo: usize, hi: usize, rng: &mut Rng, out: &mut BTreeSet<(usize, usize)>) {
    for consumer in lo.saturating_add(1)..hi {
        let count = rng.below(4);
        for _ in 0..count {
            let producer = lo + rng.below((consumer - lo) as u64) as usize;
            out.insert((producer, consumer));
        }
    }
}

fn skeleton(shape: Shape, n: usize, rng: &mut Rng) -> Vec<(usize, usize)> {
    let mut pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
    match shape {
        Shape::Chain => {
            for consumer in 1..n {
                pairs.insert((consumer - 1, consumer));
            }
        }
        Shape::Fan => {
            for consumer in 1..n {
                pairs.insert((0, consumer));
            }
        }
        Shape::Diamond => {
            let mut base = 0;
            while base + 3 < n {
                pairs.insert((base, base + 1));
                pairs.insert((base, base + 2));
                pairs.insert((base + 1, base + 3));
                pairs.insert((base + 2, base + 3));
                if base + 4 < n && rng.chance(2, 3) {
                    pairs.insert((base + 3, base + 4));
                }
                base += 4;
            }
            for consumer in base.saturating_add(1)..n {
                pairs.insert((consumer - 1, consumer));
            }
        }
        Shape::Layered => {
            let mut layer_start = 0;
            let mut previous = (0usize, 0usize);
            while layer_start < n {
                let width = 1 + rng.below(8) as usize;
                let layer_end = (layer_start + width).min(n);
                let span = previous.1 - previous.0;
                if layer_start > 0 && span > 0 {
                    for consumer in layer_start..layer_end {
                        let count = 1 + rng.below(3);
                        for _ in 0..count {
                            let producer = previous.0 + rng.below(span as u64) as usize;
                            pairs.insert((producer, consumer));
                        }
                    }
                }
                previous = (layer_start, layer_end);
                layer_start = layer_end;
            }
        }
        Shape::Sparse => sparse_edges(0, n, rng, &mut pairs),
        Shape::Disconnected => {
            let blocks = 2 + rng.below(3) as usize;
            let mut start = 0;
            for block in 0..blocks {
                let remaining = n - start;
                let take = if block + 1 == blocks {
                    remaining
                } else {
                    remaining / (blocks - block)
                };
                sparse_edges(start, start + take, rng, &mut pairs);
                start += take;
            }
        }
    }
    pairs.into_iter().collect()
}

fn pick_size(rng: &mut Rng) -> usize {
    match rng.below(100) {
        0..=4 => rng.below(4) as usize,
        5..=64 => rng.range(4, 40) as usize,
        65..=92 => rng.range(41, 120) as usize,
        _ => rng.range(121, 300) as usize,
    }
}

fn pick_weights(rng: &mut Rng, n: usize) -> Weights {
    // Saturating weights are confined to tiny graphs: the point is to reach the boundary of the
    // representable range, and a handful of units of a quarter of `u64::MAX` already does.
    if n <= 6 && rng.chance(1, 4) {
        return Weights::Extreme;
    }
    match rng.below(24) {
        0..=2 => Weights::Zero,
        3..=6 => Weights::Equal,
        7..=9 => Weights::Huge,
        _ => Weights::Mixed,
    }
}

fn spec_of_size(rng: &mut Rng, n: usize) -> GraphSpec {
    let shape = match rng.below(6) {
        0 => Shape::Chain,
        1 => Shape::Fan,
        2 => Shape::Diamond,
        3 => Shape::Layered,
        4 => Shape::Disconnected,
        _ => Shape::Sparse,
    };
    let weights = pick_weights(rng, n);
    let units: Vec<UnitSpec> = (0..n).map(|_| random_unit(rng, weights)).collect();
    let edges = skeleton(shape, n, rng)
        .into_iter()
        .map(|(producer, consumer)| {
            let kind = if units[producer].metadata_at.is_some() && rng.chance(1, 2) {
                EdgeKind::Metadata
            } else {
                EdgeKind::Full
            };
            (producer, consumer, kind)
        })
        .collect();
    GraphSpec {
        units,
        edges,
        shape,
        weights,
    }
}

/// The shared corpus: the empty graph, a single unit, a pair, and then `CORPUS_SIZE - 3` arbitrary
/// graphs. Deterministic, so graph 47 is the same graph on every run.
fn corpus() -> Vec<GraphSpec> {
    let mut rng = Rng::new(CORPUS_SEED);
    let mut specs = Vec::with_capacity(CORPUS_SIZE);
    for n in 0..3 {
        specs.push(spec_of_size(&mut rng, n));
    }
    while specs.len() < CORPUS_SIZE {
        let n = pick_size(&mut rng);
        specs.push(spec_of_size(&mut rng, n));
    }
    specs
}

/// The graphs of the shared corpus small enough for the checks that rebuild the graph once per
/// unit.
fn small_corpus() -> Vec<GraphSpec> {
    corpus()
        .into_iter()
        .filter(|s| s.units.len() <= 40)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Independent re-derivations of the definitions in sections 3 to 6
// ---------------------------------------------------------------------------------------------

fn executors(count: u32) -> NonZeroU32 {
    NonZeroU32::new(count.max(1)).unwrap()
}

/// Every unit's identifier, in identifier order, so that a bit index can be turned back into a
/// unit.
fn ids(graph: &BuildGraph) -> Vec<UnitId> {
    graph.units().map(|(id, _)| id).collect()
}

/// The transitive dependency closure of every unit, materialised as a bitset per unit.
///
/// This is the deliberately naive enumeration section 3.1 says the recurrence must agree with: it
/// unions the closures of the direct dependencies rather than tracking a single running maximum.
fn closures(graph: &BuildGraph) -> Vec<Vec<u64>> {
    let n = graph.len();
    let words = n.div_ceil(64);
    let mut sets = vec![vec![0u64; words]; n];
    for &consumer in graph.topological_order() {
        let mut acc = vec![0u64; words];
        for dep in graph.dependencies(consumer).unwrap_or(&[]) {
            let producer = dep.unit.index();
            for (slot, word) in acc.iter_mut().zip(sets[producer].iter()) {
                *slot |= *word;
            }
            acc[producer / 64] |= 1u64 << (producer % 64);
        }
        sets[consumer.index()] = acc;
    }
    sets
}

fn in_set(set: &[u64], index: usize) -> bool {
    set.get(index / 64)
        .is_some_and(|w| (w >> (index % 64)) & 1 == 1)
}

fn set_members(set: &[u64], all: &[UnitId]) -> Vec<UnitId> {
    all.iter()
        .copied()
        .filter(|id| in_set(set, id.index()))
        .collect()
}

/// When the edge `producer -> consumer` stops blocking the consumer, given when the producer
/// started. Section 3.
fn release(graph: &BuildGraph, producer: UnitId, kind: EdgeKind, producer_start: Millis) -> Millis {
    let unit = graph.unit(producer).unwrap();
    let offset = match kind {
        EdgeKind::Full => unit.total,
        EdgeKind::Metadata => unit.metadata_at.unwrap(),
    };
    producer_start.saturating_add(offset)
}

/// The sum of every unit's duration, recomputed rather than read off the graph.
fn total_work(graph: &BuildGraph) -> Millis {
    graph.units().fold(Millis::ZERO, |acc, (_, unit)| {
        acc.saturating_add(unit.total)
    })
}

/// The latest start each unit could take without lengthening the critical path.
///
/// Derived directly from section 3: delaying a unit by `d` delays its metadata by `d` as well, so
/// a metadata edge constrains the producer's start by the consumer's start less `metadata_at`, and
/// a full edge by the consumer's start less `total`. A unit that requires its whole closure
/// constrains everything upstream of it, not only its neighbours, which is propagated backwards in
/// the mirror of the `maxFullEft` recurrence.
fn latest_starts(graph: &BuildGraph, analysis: &Analysis) -> Vec<Millis> {
    let n = graph.len();
    let cp = analysis.critical_path_len();
    let mut lst = vec![INF; n];
    let mut closure_bound = vec![INF; n];

    for &id in graph.topological_order().iter().rev() {
        let unit = graph.unit(id).unwrap();
        let mut latest = cp.saturating_sub(unit.total);
        let mut downstream_closure = INF;

        for dependent in graph.dependents(id).unwrap_or(&[]) {
            let consumer = dependent.unit;
            let consumer_start = lst[consumer.index()];
            let offset = match dependent.kind {
                EdgeKind::Full => unit.total,
                EdgeKind::Metadata => unit.metadata_at.unwrap(),
            };
            latest = latest.min(consumer_start.saturating_sub(offset));

            if graph.unit(consumer).unwrap().requires_full_closure {
                downstream_closure = downstream_closure.min(consumer_start);
            }
            downstream_closure = downstream_closure.min(closure_bound[consumer.index()]);
        }

        if downstream_closure != INF {
            latest = latest.min(downstream_closure.saturating_sub(unit.total));
        }
        lst[id.index()] = latest;
        closure_bound[id.index()] = downstream_closure;
    }
    lst
}

/// Everything a graph must satisfy to be a graph at all, checked on generated and on transformed
/// graphs alike.
fn assert_graph_is_valid(graph: &BuildGraph, context: &str) {
    let n = graph.len();
    assert_eq!(
        graph.is_empty(),
        n == 0,
        "{context}: emptiness disagrees with length"
    );

    let order = graph.topological_order();
    assert_eq!(
        order.len(),
        n,
        "{context}: topological order does not cover every unit"
    );
    let distinct: BTreeSet<UnitId> = order.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        n,
        "{context}: topological order repeats a unit"
    );

    let mut position = vec![usize::MAX; n];
    for (at, id) in order.iter().enumerate() {
        assert!(
            id.index() < n,
            "{context}: topological order names a foreign unit"
        );
        position[id.index()] = at;
    }

    let mut forward: BTreeSet<(usize, usize, EdgeKind)> = BTreeSet::new();
    let mut backward: BTreeSet<(usize, usize, EdgeKind)> = BTreeSet::new();
    for (id, unit) in graph.units() {
        if let Some(metadata_at) = unit.metadata_at {
            assert!(
                metadata_at <= unit.total,
                "{context}: unit {id} emits metadata after it finishes"
            );
        }
        for dep in graph.dependencies(id).unwrap_or(&[]) {
            assert_ne!(dep.unit, id, "{context}: unit {id} depends on itself");
            assert!(
                position[dep.unit.index()] < position[id.index()],
                "{context}: dependency of {id} is not ordered before it"
            );
            if dep.kind == EdgeKind::Metadata {
                assert!(
                    graph.unit(dep.unit).unwrap().metadata_at.is_some(),
                    "{context}: metadata edge leaves a unit that emits no metadata"
                );
            }
            forward.insert((dep.unit.index(), id.index(), dep.kind));
        }
        for dependent in graph.dependents(id).unwrap_or(&[]) {
            backward.insert((id.index(), dependent.unit.index(), dependent.kind));
        }
    }
    assert_eq!(
        forward, backward,
        "{context}: dependencies and dependents disagree"
    );
    assert_eq!(
        forward.len(),
        graph.edge_count(),
        "{context}: edge count disagrees"
    );
    assert_eq!(
        graph.total_work(),
        total_work(graph),
        "{context}: total work disagrees"
    );
}

// ---------------------------------------------------------------------------------------------
// The corpus itself
// ---------------------------------------------------------------------------------------------

/// What the corpus is required to contain. Each field counts graphs or units exhibiting the case.
#[derive(Debug, Default)]
struct Facts {
    empty: usize,
    single: usize,
    large: usize,
    zero_weight_units: usize,
    equal_weight_graphs: usize,
    huge_weight_units: usize,
    saturating_graphs: usize,
    opaque_units: usize,
    metadata_at_zero: usize,
    metadata_at_total: usize,
    metadata_at_between: usize,
    metadata_edges: usize,
    full_edges: usize,
    closure_units: usize,
    closure_sources: usize,
    closure_sinks: usize,
    closure_beyond_neighbours: usize,
    disconnected: usize,
    deep_chains: usize,
    wide_fans: usize,
    diamonds: usize,
    shapes: [usize; 6],
}

fn facts(specs: &[GraphSpec]) -> Facts {
    let mut facts = Facts::default();
    for spec in specs {
        let n = spec.units.len();
        facts.shapes[spec.shape.index()] += 1;
        match n {
            0 => facts.empty += 1,
            1 => facts.single += 1,
            _ => {}
        }
        if n >= 150 {
            facts.large += 1;
        }
        if spec.weights == Weights::Equal && n >= 8 {
            facts.equal_weight_graphs += 1;
        }
        if spec.weights == Weights::Extreme && n >= 2 {
            facts.saturating_graphs += 1;
        }
        for unit in &spec.units {
            if unit.total == 0 {
                facts.zero_weight_units += 1;
            }
            if unit.total >= 1_000_000_000_000 {
                facts.huge_weight_units += 1;
            }
            match unit.metadata_at {
                None => facts.opaque_units += 1,
                // A unit of no duration has its metadata at both extremes at once and says
                // nothing about either.
                Some(_) if unit.total == 0 => {}
                Some(0) => facts.metadata_at_zero += 1,
                Some(at) if at == unit.total => facts.metadata_at_total += 1,
                Some(_) => facts.metadata_at_between += 1,
            }
        }
        for &(_, _, kind) in &spec.edges {
            match kind {
                EdgeKind::Metadata => facts.metadata_edges += 1,
                EdgeKind::Full => facts.full_edges += 1,
            }
        }

        let mut out_degree = vec![0usize; n];
        let mut in_degree = vec![0usize; n];
        let mut depth = vec![0usize; n];
        let mut parent = (0..n).collect::<Vec<usize>>();
        for &(producer, consumer, _) in &spec.edges {
            out_degree[producer] += 1;
            in_degree[consumer] += 1;
            depth[consumer] = depth[consumer].max(depth[producer] + 1);
            union(&mut parent, producer, consumer);
        }
        if depth.iter().copied().max().unwrap_or(0) >= 8 {
            facts.deep_chains += 1;
        }
        if out_degree.iter().copied().max().unwrap_or(0) >= 8 {
            facts.wide_fans += 1;
        }
        if n >= 2 {
            let components: BTreeSet<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
            if components.len() >= 2 {
                facts.disconnected += 1;
            }
        }

        let graph = spec.build();
        let sets = closures(&graph);
        for (id, unit) in graph.units() {
            if !unit.requires_full_closure {
                continue;
            }
            facts.closure_units += 1;
            let direct = graph.dependencies(id).unwrap_or(&[]);
            if direct.is_empty() {
                facts.closure_sources += 1;
            }
            if graph.dependents(id).unwrap_or(&[]).is_empty() {
                facts.closure_sinks += 1;
            }
            let reachable = sets[id.index()]
                .iter()
                .map(|w| w.count_ones() as usize)
                .sum::<usize>();
            if reachable > direct.len() {
                facts.closure_beyond_neighbours += 1;
            }
        }

        // A diamond: one unit with two distinct dependencies whose closures, taken with the
        // dependencies themselves, meet.
        for (id, _) in graph.units() {
            let deps = graph.dependencies(id).unwrap_or(&[]);
            let mut found = false;
            for (i, left) in deps.iter().enumerate() {
                for right in deps.iter().skip(i + 1) {
                    let mut a = sets[left.unit.index()].clone();
                    let mut b = sets[right.unit.index()].clone();
                    a[left.unit.index() / 64] |= 1u64 << (left.unit.index() % 64);
                    b[right.unit.index() / 64] |= 1u64 << (right.unit.index() % 64);
                    if a.iter().zip(b.iter()).any(|(x, y)| x & y != 0) {
                        found = true;
                    }
                }
            }
            if found {
                facts.diamonds += 1;
                break;
            }
        }
    }
    facts
}

fn find(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[ra] = rb;
    }
}

#[test]
fn corpus_is_non_trivial() {
    let facts = facts(&corpus());

    // Sizes.
    assert!(facts.empty >= 1, "no empty graph: {facts:?}");
    assert!(facts.single >= 1, "no single-unit graph: {facts:?}");
    assert!(
        facts.large >= 2,
        "nothing of a few hundred units: {facts:?}"
    );

    // Weights.
    assert!(
        facts.zero_weight_units >= 20,
        "too few zero weights: {facts:?}"
    );
    assert!(
        facts.equal_weight_graphs >= 5,
        "too few all-equal graphs: {facts:?}"
    );
    assert!(
        facts.huge_weight_units >= 20,
        "too few very large weights: {facts:?}"
    );
    assert!(
        facts.saturating_graphs >= 2,
        "nothing near saturation: {facts:?}"
    );

    // Pipelining.
    assert!(facts.opaque_units >= 50, "too few opaque units: {facts:?}");
    assert!(
        facts.metadata_at_zero >= 20,
        "no metadata at offset zero: {facts:?}"
    );
    assert!(
        facts.metadata_at_total >= 20,
        "no metadata at the very end: {facts:?}"
    );
    assert!(
        facts.metadata_at_between >= 50,
        "no metadata strictly inside: {facts:?}"
    );
    assert!(
        facts.metadata_edges >= 200,
        "too few metadata edges: {facts:?}"
    );
    assert!(facts.full_edges >= 200, "too few full edges: {facts:?}");

    // The closure rule of section 3.1.
    assert!(
        facts.closure_units >= 100,
        "too few closure units: {facts:?}"
    );
    assert!(
        facts.closure_sources >= 5,
        "no closure unit without dependencies: {facts:?}"
    );
    assert!(
        facts.closure_sinks >= 5,
        "no closure unit without dependents: {facts:?}"
    );
    assert!(
        facts.closure_beyond_neighbours >= 30,
        "no closure unit whose closure reaches past its neighbours, \
         so I17 and I18 would have nothing to say: {facts:?}"
    );

    // Shapes.
    for shape in Shape::ALL {
        assert!(
            facts.shapes[shape.index()] >= 10,
            "the corpus barely contains the {shape:?} shape: {facts:?}"
        );
    }
    assert!(facts.disconnected >= 10, "no disconnected graph: {facts:?}");
    assert!(facts.deep_chains >= 10, "no deep chain: {facts:?}");
    assert!(facts.wide_fans >= 5, "no wide fan-out: {facts:?}");
    assert!(facts.diamonds >= 10, "no diamond: {facts:?}");
}

// ---------------------------------------------------------------------------------------------
// I1 to I18
// ---------------------------------------------------------------------------------------------

/// I1 — Determinism. "The same graph yields identical output across runs and platforms, with no
/// dependence on hash iteration order."
///
/// The two builds differ in the order their edges are inserted and in the capacity they reserve,
/// which is the input a hash-ordered implementation would react to.
#[test]
fn i1_the_same_graph_yields_identical_answers_whatever_order_it_was_assembled_in() {
    let mut compared = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let first = spec.build();

        let mut reversed = spec.clone();
        reversed.edges.reverse();
        let second = reversed.build();

        let a = analyze(&first);
        let b = analyze(&second);
        assert_eq!(
            a.critical_path_len(),
            b.critical_path_len(),
            "graph {index}: critical path length differs between two assemblies"
        );
        assert_eq!(
            a.critical_path(),
            b.critical_path(),
            "graph {index}: critical path differs between two assemblies"
        );
        for (id, _) in first.units() {
            assert_eq!(
                a.earliest_start(id),
                b.earliest_start(id),
                "graph {index}, unit {id}"
            );
            assert_eq!(
                a.earliest_finish(id),
                b.earliest_finish(id),
                "graph {index}, unit {id}"
            );
            assert_eq!(
                a.latest_finish(id),
                b.latest_finish(id),
                "graph {index}, unit {id}"
            );
            assert_eq!(a.slack(id), b.slack(id), "graph {index}, unit {id}");
            assert_eq!(
                a.is_critical(id),
                b.is_critical(id),
                "graph {index}, unit {id}"
            );
        }

        for policy in POLICIES {
            for count in EXECUTORS {
                let x = simulate(&first, executors(count), policy);
                let y = simulate(&second, executors(count), policy);
                assert_eq!(
                    x.makespan(),
                    y.makespan(),
                    "graph {index}: makespan at {count} executors under {policy:?} \
                     depends on assembly order"
                );
                for (id, _) in first.units() {
                    assert_eq!(
                        x.start(id),
                        y.start(id),
                        "graph {index}, unit {id}: start at {count} executors under {policy:?} \
                         depends on assembly order"
                    );
                }
                compared += 1;
            }
        }
    }
    assert_eq!(compared, CORPUS_SIZE * POLICIES.len() * EXECUTORS.len());
}

/// I2 — Precedence respected. "For every edge `e = (D -> U)` in a produced schedule,
/// `start(U) >= release(e)`." Section 3.1 extends readiness past the neighbours for a unit that
/// requires its whole closure, so that is checked here too.
#[test]
fn i2_no_unit_starts_before_the_edges_into_it_release_it() {
    let mut edges_checked = 0usize;
    let mut closure_units_checked = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let sets = closures(&graph);
        let all = ids(&graph);
        for policy in POLICIES {
            for count in EXECUTORS {
                let schedule = simulate(&graph, executors(count), policy);
                for (id, unit) in graph.units() {
                    let start = schedule.start(id).unwrap();
                    for dep in graph.dependencies(id).unwrap_or(&[]) {
                        let producer_start = schedule.start(dep.unit).unwrap();
                        let released = release(&graph, dep.unit, dep.kind, producer_start);
                        assert!(
                            start >= released,
                            "graph {index}: unit {id} starts at {start} but the {} edge from {} \
                             releases it at {released} ({count} executors, {policy:?})",
                            dep.kind,
                            dep.unit
                        );
                        edges_checked += 1;
                    }
                    if unit.requires_full_closure {
                        for producer in set_members(&sets[id.index()], &all) {
                            let finish = schedule.finish(producer).unwrap();
                            assert!(
                                start >= finish,
                                "graph {index}: unit {id} requires its closure but starts at \
                                 {start}, before {producer} finishes at {finish} \
                                 ({count} executors, {policy:?})"
                            );
                            closure_units_checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        edges_checked > 10_000,
        "hardly any edges were checked: {edges_checked}"
    );
    assert!(
        closure_units_checked > 1_000,
        "hardly any closure constraints were checked: {closure_units_checked}"
    );
}

/// I3 — Capacity respected. "At no instant are more than `J` units running."
///
/// A unit of zero duration occupies `[start, start)`, which contains no instant, so it is not
/// running at any instant and cannot contribute to the count.
#[test]
fn i3_no_more_than_j_units_are_running_at_any_instant() {
    let mut saw_full_occupancy = false;
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        for policy in POLICIES {
            for count in EXECUTORS {
                let schedule = simulate(&graph, executors(count), policy);
                let mut events: Vec<(Millis, i64)> = Vec::new();
                for (id, unit) in graph.units() {
                    if unit.total == Millis::ZERO {
                        continue;
                    }
                    let start = schedule.start(id).unwrap();
                    events.push((start, 1));
                    events.push((start.saturating_add(unit.total), -1));
                }
                // A unit finishing at `t` frees its executor for one starting at `t`, so the
                // departures at an instant are applied before the arrivals.
                events.sort_by_key(|&(at, delta)| (at, delta));
                let mut running = 0i64;
                for (at, delta) in events {
                    running += delta;
                    assert!(
                        running <= i64::from(count),
                        "graph {index}: {running} units running at {at} on {count} executors \
                         under {policy:?}"
                    );
                    if running == i64::from(count) && count > 1 {
                        saw_full_occupancy = true;
                    }
                }
            }
        }
    }
    assert!(
        saw_full_occupancy,
        "no schedule ever filled more than one executor, so the capacity check proved nothing"
    );
}

/// I4 — Non-preemption. "Each unit occupies one executor over exactly
/// `[start(U), start(U) + total(U))`."
#[test]
fn i4_every_unit_occupies_exactly_its_own_duration() {
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        for policy in POLICIES {
            for count in EXECUTORS {
                let schedule = simulate(&graph, executors(count), policy);
                for (id, unit) in graph.units() {
                    let start = schedule.start(id).expect("every unit is scheduled");
                    let finish = schedule.finish(id).expect("every unit is scheduled");
                    assert_eq!(
                        finish,
                        start.saturating_add(unit.total),
                        "graph {index}: unit {id} does not occupy one contiguous interval of its \
                         own length ({count} executors, {policy:?})"
                    );
                    assert!(
                        finish <= schedule.makespan(),
                        "graph {index}: unit {id} finishes after the makespan"
                    );
                }
                assert_eq!(schedule.executors(), executors(count));
                assert_eq!(schedule.policy(), policy);
            }
        }
    }
}

/// I5 — Precedence bound. "`makespan(J) >= CP` for all `J`."
#[test]
fn i5_no_schedule_finishes_before_the_critical_path() {
    let mut strictly_above = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let cp = analyze(&graph).critical_path_len();
        for policy in POLICIES {
            for count in EXECUTORS {
                let makespan = simulate(&graph, executors(count), policy).makespan();
                assert!(
                    makespan >= cp,
                    "graph {index}: makespan {makespan} is below the critical path {cp} \
                     ({count} executors, {policy:?})"
                );
                if makespan > cp {
                    strictly_above += 1;
                }
            }
        }
    }
    assert!(
        strictly_above > 50,
        "every schedule in the corpus already sat exactly on the critical path, \
         so the bound was never load-bearing: {strictly_above}"
    );
}

/// I6 — Area bound. "`makespan(J) >= total_work / J` for all `J`."
#[test]
fn i6_no_schedule_finishes_before_the_work_can_be_done() {
    let mut binding = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let work = total_work(&graph);
        let analysis = analyze(&graph);
        for count in EXECUTORS {
            // Rounded up: the bound must be one no schedule can beat.
            let bound = Millis::new(work.get().div_ceil(u64::from(count)));
            assert_eq!(
                area_bound(&graph, executors(count)),
                bound,
                "graph {index}: the reported area bound at {count} executors is not \
                 total work divided by the executor count"
            );
            let lower = lower_bound(&graph, &analysis, executors(count));
            assert_eq!(
                lower,
                bound.max(analysis.critical_path_len()),
                "graph {index}: the lower bound at {count} executors is not the larger of \
                 its two terms"
            );
            for policy in POLICIES {
                let makespan = simulate(&graph, executors(count), policy).makespan();
                assert!(
                    makespan >= bound,
                    "graph {index}: makespan {makespan} is below the area bound {bound} \
                     ({count} executors, {policy:?})"
                );
                if bound > analysis.critical_path_len() {
                    binding += 1;
                }
            }
        }
    }
    assert!(
        binding > 50,
        "the area bound never exceeded the critical path anywhere in the corpus, \
         so it was never the binding term: {binding}"
    );
}

/// I7 — Unlimited executors reach the critical path. "For `J >= unit count`,
/// `makespan(J) == CP` exactly."
#[test]
fn i7_with_an_executor_per_unit_the_makespan_is_the_critical_path() {
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let cp = analyze(&graph).critical_path_len();
        let n = u32::try_from(graph.len()).unwrap();
        for count in [n, n.saturating_add(1), n.saturating_add(7)] {
            for policy in POLICIES {
                let makespan = simulate(&graph, executors(count), policy).makespan();
                assert_eq!(
                    makespan,
                    cp,
                    "graph {index}: {count} executors for {} units gave makespan {makespan}, \
                     not the critical path {cp} ({policy:?})",
                    graph.len()
                );
            }
        }
    }
}

/// I9 — Slack non-negative, zero on the critical path. "With `lft(U)` the latest finish that does
/// not increase `CP`, `slack(U) = lft(U) - eft(U) >= 0`, and `slack(U) = 0` for every `U` on a
/// critical path."
///
/// The reported slack is also compared against a backward pass written from section 3 here, and
/// the recovered path is checked to be a real chain: each step is explained either by an incoming
/// edge that attains the consumer's earliest start, or, for a unit that requires its closure, by a
/// unit in that closure finishing exactly then.
#[test]
fn i9_slack_is_never_negative_and_is_zero_along_the_critical_path() {
    let mut positive_slack = 0usize;
    let mut closure_steps = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let analysis = analyze(&graph);
        let cp = analysis.critical_path_len();
        let sets = closures(&graph);
        let expected_starts = latest_starts(&graph, &analysis);

        // Once the weights are large enough that the sums saturate, the latest-finish time is a
        // difference of numbers that no longer stand for what they say, so it cannot be compared
        // against an independently derived one. It is still required to be meetable, which is
        // asserted for every graph below, saturating or not.
        let saturated = cp == INF;

        for (id, unit) in graph.units() {
            let eft = analysis.earliest_finish(id).unwrap();
            let lft = analysis.latest_finish(id).unwrap();
            let slack = analysis.slack(id).unwrap();
            // A unit can always finish at the earliest time it can, so a deadline earlier than that
            // is not a deadline. This holds whether or not the sums saturated.
            assert!(
                lft >= eft,
                "unit {id} may finish at {eft} at the earliest but is told to finish by {lft}"
            );
            assert!(
                lft <= cp,
                "graph {index}: unit {id} may finish after the critical path"
            );
            if slack > Millis::ZERO {
                positive_slack += 1;
            }
            if saturated {
                continue;
            }
            assert!(
                lft >= eft,
                "graph {index}: unit {id} has latest finish before earliest"
            );
            assert_eq!(
                slack,
                lft.saturating_sub(eft),
                "graph {index}: unit {id} slack is not its latest finish less its earliest"
            );
            assert_eq!(
                lft,
                expected_starts[id.index()].saturating_add(unit.total),
                "graph {index}: unit {id} latest finish disagrees with the backward pass"
            );
            if graph.dependents(id).unwrap_or(&[]).is_empty() {
                assert_eq!(
                    lft, cp,
                    "graph {index}: unit {id} has nothing depending on it, so it may finish as \
                     late as the critical path"
                );
            }
        }

        let path = analysis.critical_path();
        assert_eq!(
            path.is_empty(),
            graph.is_empty(),
            "graph {index}: path emptiness is wrong"
        );
        let distinct: BTreeSet<UnitId> = path.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            path.len(),
            "graph {index}: the critical path repeats a unit"
        );

        for &id in path {
            assert_eq!(
                analysis.slack(id).unwrap(),
                Millis::ZERO,
                "graph {index}: unit {id} is on the critical path with slack to spare"
            );
            assert!(
                analysis.is_critical(id),
                "graph {index}: unit {id} is on the path but not critical"
            );
        }
        for (id, _) in graph.units() {
            if analysis.is_critical(id) {
                assert_eq!(
                    analysis.slack(id).unwrap(),
                    Millis::ZERO,
                    "graph {index}: unit {id} is critical with slack to spare"
                );
            }
        }

        if let Some(&last) = path.last() {
            let &first = path.first().unwrap();
            let ends_at_cp = analysis.earliest_finish(last).unwrap() == cp
                || analysis.earliest_finish(first).unwrap() == cp;
            assert!(
                ends_at_cp,
                "graph {index}: neither end of the critical path attains it"
            );

            // Orient the path from source to sink, then explain every step.
            let mut chain: Vec<UnitId> = path.to_vec();
            if analysis.earliest_finish(first).unwrap() == cp && path.len() > 1 {
                chain.reverse();
            }
            for pair in chain.windows(2) {
                let (producer, consumer) = (pair[0], pair[1]);
                assert!(
                    in_set(&sets[consumer.index()], producer.index()),
                    "graph {index}: {producer} precedes {consumer} on the critical path but is \
                     not in its dependency closure"
                );
                let est = analysis.earliest_start(consumer).unwrap();
                let direct = graph
                    .dependencies(consumer)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|d| d.unit == producer)
                    .any(|d| {
                        release(
                            &graph,
                            producer,
                            d.kind,
                            analysis.earliest_start(producer).unwrap(),
                        ) == est
                    });
                let by_closure = graph.unit(consumer).unwrap().requires_full_closure
                    && analysis.earliest_finish(producer).unwrap() == est;
                if by_closure && !direct {
                    closure_steps += 1;
                }
                assert!(
                    direct || by_closure,
                    "graph {index}: nothing explains why {consumer} starts at {est} behind \
                     {producer}"
                );
            }
            // Section 4 traces back "until a unit with no dependencies is reached".
            let head = chain[0];
            assert!(
                graph.dependencies(head).unwrap_or(&[]).is_empty(),
                "graph {index}: the critical path starts at {head}, which has dependencies of \
                 its own"
            );
            assert_eq!(analysis.earliest_start(head), Some(Millis::ZERO));
        }
    }
    assert!(
        positive_slack > 500,
        "almost nothing had slack: {positive_slack}"
    );
    assert!(
        closure_steps > 0,
        "no critical path step was ever explained by the closure rule rather than by a direct \
         edge, so that branch went unchecked"
    );
}

/// I10 — `CP` is independent of `J`. "Computing `CP` never consults `J`."
///
/// The signature already withholds `J` from `analyze`, so what is checked is the observable
/// consequence: interleaving simulations at every executor count leaves the answer alone, on
/// graphs whose makespan demonstrably does move with `J`.
#[test]
fn i10_the_critical_path_does_not_move_with_the_executor_count() {
    let mut makespan_moved = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let cp = analyze(&graph).critical_path_len();
        let mut seen = BTreeSet::new();
        for count in 1..=12u32 {
            let makespan = simulate(&graph, executors(count), Policy::CargoLike).makespan();
            seen.insert(makespan);
            assert_eq!(
                analyze(&graph).critical_path_len(),
                cp,
                "graph {index}: the critical path changed after simulating {count} executors"
            );
        }
        if seen.len() > 1 {
            makespan_moved += 1;
        }
    }
    assert!(
        makespan_moved > 40,
        "the executor count changed nothing anywhere in the corpus, so holding CP fixed proved \
         nothing: {makespan_moved}"
    );
}

/// I11 — Single unit. "`CP = total(U)` and `makespan(J) = total(U)` for all `J >= 1`."
#[test]
fn i11_a_lone_unit_is_the_whole_build() {
    let cases: [(u64, Option<u64>, bool, UnitKind); 6] = [
        (0, None, false, UnitKind::Lib),
        (0, Some(0), true, UnitKind::Bin),
        (1, Some(0), false, UnitKind::Lib),
        (4_321, Some(4_321), false, UnitKind::Lib),
        (4_321, Some(1_000), true, UnitKind::ProcMacro),
        (u64::MAX, Some(u64::MAX), true, UnitKind::Test),
    ];
    for (total, metadata_at, closure, kind) in cases {
        let spec = GraphSpec {
            units: vec![UnitSpec {
                total,
                metadata_at,
                closure,
                kind,
            }],
            edges: Vec::new(),
            shape: Shape::Chain,
            weights: Weights::Mixed,
        };
        let graph = spec.build();
        let analysis = analyze(&graph);
        let expected = Millis::new(total);
        assert_eq!(
            analysis.critical_path_len(),
            expected,
            "single unit of {total}ms"
        );
        assert_eq!(analysis.critical_path().len(), 1);
        assert_eq!(
            analysis.earliest_start(graph.topological_order()[0]),
            Some(Millis::ZERO)
        );
        for count in 1..=9u32 {
            for policy in POLICIES {
                assert_eq!(
                    simulate(&graph, executors(count), policy).makespan(),
                    expected,
                    "single unit of {total}ms on {count} executors under {policy:?}"
                );
            }
        }
    }
}

/// I12 — Empty graph. "`CP = 0`, `makespan = 0`, no panic and no division by zero."
#[test]
fn i12_an_empty_graph_answers_zero_rather_than_failing() {
    let graph = GraphBuilder::new().build().unwrap();
    assert!(graph.is_empty());
    assert_eq!(graph.len(), 0);
    assert_eq!(graph.edge_count(), 0);
    assert_eq!(graph.total_work(), Millis::ZERO);
    assert!(graph.topological_order().is_empty());
    assert!(roots(&graph).is_empty());

    let analysis = analyze(&graph);
    assert_eq!(analysis.critical_path_len(), Millis::ZERO);
    assert!(analysis.critical_path().is_empty());

    for count in [1u32, 2, 7, u32::MAX] {
        assert_eq!(area_bound(&graph, executors(count)), Millis::ZERO);
        assert_eq!(
            lower_bound(&graph, &analysis, executors(count)),
            Millis::ZERO
        );
        for policy in POLICIES {
            assert_eq!(
                simulate(&graph, executors(count), policy).makespan(),
                Millis::ZERO
            );
        }
    }

    let applied = apply(&graph, &Transform::RemoveUnits { units: Vec::new() }).unwrap();
    assert!(applied.graph().is_empty());
    assert_eq!(applied.mapping().len(), 0);
}

/// I13 — Metadata edges cannot lengthen a path. "Reclassifying any `Full` edge as `Metadata`
/// cannot increase `CP`, because `metadata_at <= total`."
#[test]
fn i13_turning_full_edges_into_metadata_edges_never_lengthens_the_critical_path() {
    let mut shortened = 0usize;
    let mut reclassified = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let before = spec.critical_path_len();

        let mut relaxed = spec.clone();
        let mut changed = 0usize;
        for edge in &mut relaxed.edges {
            if edge.2 == EdgeKind::Full && spec.units[edge.0].metadata_at.is_some() {
                edge.2 = EdgeKind::Metadata;
                changed += 1;
            }
        }
        if changed == 0 {
            continue;
        }
        reclassified += changed;
        let after = relaxed.critical_path_len();
        assert!(
            after <= before,
            "graph {index}: reclassifying {changed} full edges as metadata edges took the \
             critical path from {before} to {after}"
        );
        if after < before {
            shortened += 1;
        }
    }
    assert!(
        reclassified > 200,
        "hardly any edge was reclassified: {reclassified}"
    );
    assert!(
        shortened > 20,
        "reclassification never once shortened anything, so the inequality was trivial: {shortened}"
    );
}

/// I14 — Weight monotonicity. "Increasing any `total(U)` cannot decrease `CP`."
#[test]
fn i14_making_a_unit_slower_never_shortens_the_critical_path() {
    let mut lengthened = 0usize;
    let mut trials = 0usize;
    let mut rng = Rng::new(CORPUS_SEED ^ 0x14);
    for (index, spec) in corpus().iter().enumerate() {
        if spec.units.is_empty() {
            continue;
        }
        let before = spec.critical_path_len();
        for _ in 0..4 {
            let target = rng.below(spec.units.len() as u64) as usize;
            let extra = match rng.below(3) {
                0 => 1,
                1 => 997,
                _ => 1_000_000,
            };
            let mut heavier = spec.clone();
            heavier.units[target].total = heavier.units[target].total.saturating_add(extra);
            let after = heavier.critical_path_len();
            assert!(
                after >= before,
                "graph {index}: adding {extra}ms to unit {target} took the critical path from \
                 {before} down to {after}"
            );
            if after > before {
                lengthened += 1;
            }
            trials += 1;
        }
    }
    assert!(trials > 400, "too few weight increases tried: {trials}");
    assert!(
        lengthened > 100,
        "no weight increase ever reached the critical path: {lengthened}"
    );
}

/// I15 — Chain and fan closed forms. "For a chain of `n` units of weight `w` joined by `Full`
/// edges, `CP = n*w`. For one source fanning out to `n` sinks, `CP = w_source + max w_sink`."
///
/// The pipelined chain is the same closed form read off section 3: a metadata edge lets the path
/// traverse a unit for `metadata_at` rather than for `total`.
#[test]
fn i15_chains_and_fans_match_their_closed_forms() {
    for n in [1usize, 2, 3, 7, 40] {
        let w = 130u64;

        let chain = GraphSpec {
            units: (0..n)
                .map(|_| UnitSpec {
                    total: w,
                    metadata_at: Some(50),
                    closure: false,
                    kind: UnitKind::Lib,
                })
                .collect(),
            edges: (1..n).map(|i| (i - 1, i, EdgeKind::Full)).collect(),
            shape: Shape::Chain,
            weights: Weights::Equal,
        };
        let expected = Millis::new(n as u64 * w);
        assert_eq!(
            chain.critical_path_len(),
            expected,
            "a chain of {n} units of {w}ms joined by full edges takes {n} times {w}ms"
        );
        assert_eq!(analyze(&chain.build()).critical_path().len(), n);

        // The same chain over metadata edges: every unit but the last contributes only the 50ms
        // it takes to reach its metadata.
        let mut pipelined = chain.clone();
        for edge in &mut pipelined.edges {
            edge.2 = EdgeKind::Metadata;
        }
        assert_eq!(
            pipelined.critical_path_len(),
            Millis::new((n as u64 - 1) * 50 + w),
            "a pipelined chain of {n} units contributes one metadata offset per link"
        );

        // The same chain again, with the last unit needing its whole closure. Nothing changes:
        // every edge is already a full edge, so the closure is already accounted for.
        let mut linking = chain.clone();
        linking.units[n - 1].closure = true;
        assert_eq!(linking.critical_path_len(), expected);

        // But over metadata edges the closure requirement pulls the last unit back to the point
        // where everything ahead of it has finished: unit i finishes at 50*i + 130.
        let mut pipelined_linking = pipelined.clone();
        pipelined_linking.units[n - 1].closure = true;
        let latest_upstream = if n == 1 { 0 } else { 50 * (n as u64 - 2) + w };
        assert_eq!(
            pipelined_linking.critical_path_len(),
            Millis::new(latest_upstream + w),
            "a linking unit at the end of a pipelined chain of {n} waits for all of it"
        );
    }

    for n in [1usize, 2, 5, 30] {
        let source = 700u64;
        // Sink weights ascending, so the maximum is the last and is not the first tried.
        let sinks: Vec<u64> = (0..n).map(|i| 10 + 3 * i as u64).collect();
        let mut units = vec![UnitSpec {
            total: source,
            metadata_at: None,
            closure: false,
            kind: UnitKind::Lib,
        }];
        units.extend(sinks.iter().map(|&total| UnitSpec {
            total,
            metadata_at: None,
            closure: false,
            kind: UnitKind::Lib,
        }));
        let fan = GraphSpec {
            units,
            edges: (1..=n).map(|i| (0, i, EdgeKind::Full)).collect(),
            shape: Shape::Fan,
            weights: Weights::Mixed,
        };
        let expected = Millis::new(source + sinks.iter().copied().max().unwrap());
        assert_eq!(
            fan.critical_path_len(),
            expected,
            "one source of {source}ms fanning out to {n} sinks takes the source plus the \
             slowest sink"
        );
        assert_eq!(analyze(&fan.build()).critical_path().len(), 2);
    }
}

/// I16 — A closure requirement can only delay. "Setting `requires_full_closure` on any unit cannot
/// decrease `CP`, and clearing it on any unit cannot increase `CP`."
#[test]
fn i16_a_closure_requirement_only_ever_delays() {
    let mut delayed = 0usize;
    let mut released = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        if spec.units.is_empty() {
            continue;
        }
        let before = spec.critical_path_len();

        let mut all_set = spec.clone();
        for unit in &mut all_set.units {
            unit.closure = true;
        }
        let with_all = all_set.critical_path_len();
        assert!(
            with_all >= before,
            "graph {index}: requiring every closure took the critical path from {before} \
             to {with_all}"
        );
        if with_all > before {
            delayed += 1;
        }

        let mut all_clear = spec.clone();
        for unit in &mut all_clear.units {
            unit.closure = false;
        }
        let without_any = all_clear.critical_path_len();
        assert!(
            without_any <= before,
            "graph {index}: clearing every closure took the critical path from {before} \
             to {without_any}"
        );
        if without_any < before {
            released += 1;
        }

        // One unit at a time, so that the effect is attributable.
        for target in 0..spec.units.len().min(6) {
            let mut set_one = spec.clone();
            set_one.units[target].closure = true;
            assert!(
                set_one.critical_path_len() >= before,
                "graph {index}: setting the closure requirement on unit {target} shortened the \
                 critical path"
            );
            let mut clear_one = spec.clone();
            clear_one.units[target].closure = false;
            assert!(
                clear_one.critical_path_len() <= before,
                "graph {index}: clearing the closure requirement on unit {target} lengthened the \
                 critical path"
            );
        }
    }
    assert!(
        delayed > 20,
        "requiring closures never delayed anything: {delayed}"
    );
    assert!(
        released > 20,
        "clearing closures never released anything: {released}"
    );
}

/// I17 — A closure requirement subsumes direct edges. "For a unit `U` with `requires_full_closure`,
/// `est(U) >= eft(D)` for every `D` in its transitive dependency closure, not merely for its direct
/// dependencies."
#[test]
fn i17_a_closure_requirement_reaches_past_the_neighbours() {
    let mut beyond_neighbours = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let analysis = analyze(&graph);
        let sets = closures(&graph);
        let all = ids(&graph);
        for (id, unit) in graph.units() {
            if !unit.requires_full_closure {
                continue;
            }
            let est = analysis.earliest_start(id).unwrap();
            let direct: BTreeSet<UnitId> = graph
                .dependencies(id)
                .unwrap_or(&[])
                .iter()
                .map(|d| d.unit)
                .collect();
            for producer in set_members(&sets[id.index()], &all) {
                let eft = analysis.earliest_finish(producer).unwrap();
                assert!(
                    est >= eft,
                    "graph {index}: unit {id} requires its closure but starts at {est}, before \
                     {producer} in that closure finishes at {eft}"
                );
                if !direct.contains(&producer) {
                    beyond_neighbours += 1;
                }
            }
        }
    }
    assert!(
        beyond_neighbours > 500,
        "every closure in the corpus was just the direct dependencies, so the invariant said \
         nothing a per-edge rule would not have said: {beyond_neighbours}"
    );
}

/// I18 — The recurrence equals the enumerated closure. "`maxFullEft(U)` computed by the recurrence
/// of section 3.1 equals `max over D in transitiveDeps(U) of eft(D)`, computed by explicit
/// enumeration."
///
/// `maxFullEft` is not exposed, but section 3.1 makes `est(U) = maxFullEft(U)` for a unit that
/// requires its closure, and a unit's own flag does not change the earliest finish of anything
/// upstream of it. So the recurrence is read off by setting the flag on one unit at a time and
/// comparing against the enumeration performed on the original graph.
#[test]
fn i18_the_closure_recurrence_agrees_with_an_enumerated_closure() {
    let mut compared = 0usize;
    let mut non_empty_closures = 0usize;
    for (index, spec) in small_corpus().iter().enumerate() {
        let graph = spec.build();
        let analysis = analyze(&graph);
        let sets = closures(&graph);
        let all = ids(&graph);

        for (id, _) in graph.units() {
            let enumerated = set_members(&sets[id.index()], &all)
                .into_iter()
                .map(|d| analysis.earliest_finish(d).unwrap())
                .fold(Millis::ZERO, Millis::max);

            let mut forced = spec.clone();
            forced.units[id.index()].closure = true;
            let forced_graph = forced.build();
            let forced_analysis = analyze(&forced_graph);
            let recurrence = forced_analysis.earliest_start(id).unwrap();

            assert_eq!(
                recurrence, enumerated,
                "graph {index}: the earliest start of unit {id} under a closure requirement is \
                 {recurrence}, but the latest finish anywhere in its enumerated closure is \
                 {enumerated}"
            );
            compared += 1;
            if enumerated > Millis::ZERO {
                non_empty_closures += 1;
            }
        }
    }
    assert!(compared > 500, "too few units compared: {compared}");
    assert!(
        non_empty_closures > 200,
        "almost every closure was empty or free, so the recurrence was never exercised: \
         {non_empty_closures}"
    );
}

// ---------------------------------------------------------------------------------------------
// Section 9: the counterfactual transformations
// ---------------------------------------------------------------------------------------------

/// Picks a unit that can be split, an edge that exists, and a unit worth removing.
fn transform_targets(
    graph: &BuildGraph,
    rng: &mut Rng,
) -> (Option<UnitId>, Option<(UnitId, UnitId)>) {
    let splittable: Vec<UnitId> = graph
        .units()
        .filter(|(_, unit)| {
            matches!(
                unit.kind,
                UnitKind::Lib | UnitKind::Bin | UnitKind::Test | UnitKind::ProcMacro
            )
        })
        .map(|(id, _)| id)
        .collect();
    let edges: Vec<(UnitId, UnitId)> = graph
        .units()
        .flat_map(|(id, _)| {
            graph
                .dependencies(id)
                .unwrap_or(&[])
                .iter()
                .map(move |dep| (dep.unit, id))
        })
        .collect();

    let unit = if splittable.is_empty() {
        None
    } else {
        Some(splittable[rng.below(splittable.len() as u64) as usize])
    };
    let edge = if edges.is_empty() {
        None
    } else {
        Some(edges[rng.below(edges.len() as u64) as usize])
    };
    (unit, edge)
}

/// Section 9. "Each transformation produces a new graph, and the metrics of sections 4 to 6 are
/// recomputed on it." A transformation that reports success must therefore hand back a graph that
/// is a graph: acyclic, consistently linked in both directions, and with a total mapping.
#[test]
fn every_applied_transformation_yields_a_valid_graph() {
    let mut splits = 0usize;
    let mut edge_removals = 0usize;
    let mut unit_removals = 0usize;
    let mut rng = Rng::new(CORPUS_SEED ^ 0x9A);

    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        assert_graph_is_valid(&graph, &format!("graph {index}"));
        let (splittable, edge) = transform_targets(&graph, &mut rng);

        if let Some(unit) = splittable {
            let transform = Transform::SplitUnit {
                unit,
                numerator: rng.range(0, 4),
                denominator: NonZeroU64::new(4).unwrap(),
                dependents: if rng.chance(1, 2) {
                    SplitDependents::Optimistic
                } else {
                    SplitDependents::Pessimistic
                },
                overhead: Millis::new(rng.below(200)),
            };
            let applied = apply(&graph, &transform).unwrap();
            assert_graph_is_valid(applied.graph(), &format!("graph {index} after a split"));
            assert_eq!(applied.graph().len(), graph.len() + 1);
            assert_eq!(applied.mapping().len(), graph.len());
            match applied.mapped(unit).unwrap() {
                Mapped::Split { first, second } => assert_ne!(first, second),
                other => panic!("graph {index}: splitting unit {unit} reported {other:?}"),
            }
            splits += 1;
        }

        if let Some((from, to)) = edge {
            let applied = apply(&graph, &Transform::RemoveEdge { from, to }).unwrap();
            assert_graph_is_valid(
                applied.graph(),
                &format!("graph {index} after an edge removal"),
            );
            assert_eq!(applied.graph().len(), graph.len());
            assert_eq!(applied.graph().edge_count(), graph.edge_count() - 1);
            edge_removals += 1;
        }

        if !graph.is_empty() {
            let victim = ids(&graph)[rng.below(graph.len() as u64) as usize];
            let applied = apply(
                &graph,
                &Transform::RemoveUnits {
                    units: vec![victim, victim],
                },
            )
            .unwrap();
            assert_graph_is_valid(
                applied.graph(),
                &format!("graph {index} after a unit removal"),
            );
            assert!(applied.graph().len() < graph.len());
            assert_eq!(applied.mapped(victim), Some(Mapped::Removed));
            unit_removals += 1;
        }
    }
    assert!(splits > 100, "too few splits applied: {splits}");
    assert!(
        edge_removals > 100,
        "too few edge removals applied: {edge_removals}"
    );
    assert!(
        unit_removals > 100,
        "too few unit removals applied: {unit_removals}"
    );
}

/// Section 9.3, and the note in section 7 that `CP` "is monotone under edge removal". Removing
/// units removes work and constraints and can therefore only shorten the longest path.
#[test]
fn removing_units_never_lengthens_the_critical_path() {
    let mut shortened = 0usize;
    let mut rng = Rng::new(CORPUS_SEED ^ 0x93);
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        if graph.is_empty() {
            continue;
        }
        let before = analyze(&graph).critical_path_len();
        let all = ids(&graph);
        for _ in 0..3 {
            let victim = all[rng.below(all.len() as u64) as usize];
            let applied = apply(
                &graph,
                &Transform::RemoveUnits {
                    units: vec![victim],
                },
            )
            .unwrap();
            let after = analyze(applied.graph()).critical_path_len();
            assert!(
                after <= before,
                "graph {index}: removing unit {victim} took the critical path from {before} \
                 to {after}"
            );
            if after < before {
                shortened += 1;
            }
        }
    }
    assert!(
        shortened > 100,
        "removing a unit never shortened anything: {shortened}"
    );
}

/// Section 9.2. "The `CP` change is monotone and anchors the answer."
#[test]
fn removing_an_edge_never_lengthens_the_critical_path() {
    let mut shortened = 0usize;
    let mut removed = 0usize;
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let before = analyze(&graph).critical_path_len();
        for (to, _) in graph.units() {
            for dep in graph.dependencies(to).unwrap_or(&[]) {
                let applied = apply(&graph, &Transform::RemoveEdge { from: dep.unit, to }).unwrap();
                let after = analyze(applied.graph()).critical_path_len();
                assert!(
                    after <= before,
                    "graph {index}: removing the edge {} -> {to} took the critical path from \
                     {before} to {after}",
                    dep.unit
                );
                if after < before {
                    shortened += 1;
                }
                removed += 1;
            }
            if removed > 4_000 {
                break;
            }
        }
    }
    assert!(removed > 1_000, "too few edges removed: {removed}");
    assert!(
        shortened > 100,
        "removing an edge never shortened anything: {shortened}"
    );
}

/// Section 9.1. The two parts of a split "sum to exactly the original weight", and the declared
/// overhead is the only thing added, so the total work of the new graph is the old total plus twice
/// the overhead and never more.
#[test]
fn a_split_adds_no_work_beyond_the_overhead_it_charges() {
    let mut zero_overhead_splits = 0usize;
    let mut charged_splits = 0usize;
    let mut rng = Rng::new(CORPUS_SEED ^ 0x91);
    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        let (splittable, _) = transform_targets(&graph, &mut rng);
        let Some(unit) = splittable else { continue };
        let before = total_work(&graph);

        for overhead in [0u64, 250] {
            for numerator in [0u64, 1, 2, 3, 4] {
                let applied = apply(
                    &graph,
                    &Transform::SplitUnit {
                        unit,
                        numerator,
                        denominator: NonZeroU64::new(4).unwrap(),
                        dependents: SplitDependents::Optimistic,
                        overhead: Millis::new(overhead),
                    },
                )
                .unwrap();
                let after = total_work(applied.graph());
                let charged = Millis::new(overhead.saturating_mul(2));
                assert!(
                    after <= before.saturating_add(charged),
                    "graph {index}: splitting unit {unit} at {numerator}/4 with {overhead}ms of \
                     overhead each took total work from {before} to {after}, more than the \
                     {charged} it charged"
                );
                assert!(
                    after >= before,
                    "graph {index}: splitting unit {unit} at {numerator}/4 lost work: \
                     {before} became {after}"
                );
                if overhead == 0 {
                    assert_eq!(
                        after, before,
                        "graph {index}: a free split of unit {unit} at {numerator}/4 changed the \
                         total work"
                    );
                    zero_overhead_splits += 1;
                } else {
                    charged_splits += 1;
                }
            }
        }
    }
    assert!(
        zero_overhead_splits > 200,
        "too few free splits: {zero_overhead_splits}"
    );
    assert!(
        charged_splits > 200,
        "too few charged splits: {charged_splits}"
    );
}

// ---------------------------------------------------------------------------------------------
// Scheduling anomalies: cases where a greedy list schedule moves the wrong way
// ---------------------------------------------------------------------------------------------

/// Eleven units, five of them a chain and six of them independent filler, all joined by full
/// edges. This shape is the counterexample behind the two anomaly tests below.
///
///     6 -> 7 -> 10
///     6 -> 8 -> 9 -> 10          0 1 2 3 4 5 stand alone
///
/// Weights in milliseconds: 0:2800 1:3900 2:3200 3:4500 4:4100 5:3600 6:4400 7:4700 8:4600
/// 9:300 10:4900. Nothing is pipelined and nothing requires its closure, so the only mechanism
/// at work is the greedy list schedule of section 5.
fn graham_shape() -> GraphSpec {
    let totals = [
        2800, 3900, 3200, 4500, 4100, 3600, 4400, 4700, 4600, 300, 4900,
    ];
    GraphSpec {
        units: totals
            .iter()
            .map(|&total| UnitSpec {
                total,
                metadata_at: None,
                closure: false,
                kind: UnitKind::Lib,
            })
            .collect(),
        edges: vec![
            (6, 7, EdgeKind::Full),
            (6, 8, EdgeKind::Full),
            (7, 10, EdgeKind::Full),
            (8, 9, EdgeKind::Full),
            (9, 10, EdgeKind::Full),
        ],
        shape: Shape::Sparse,
        weights: Weights::Mixed,
    }
}

/// I8 — Monotonicity in J. "`makespan(J+1) <= makespan(J)`. This holds for the scheduler as
/// specified only because the priority is a fixed total order that does not depend on `J`."
///
/// **The invariant is false as stated, and the reason given for it does not hold.** A fixed
/// priority list is exactly the setting of Graham's 1969 anomaly: adding a processor changes which
/// unit is running when a later one becomes ready, and a greedy scheduler cannot undo that. The
/// graph below is an eleven-unit counterexample under `cargo-like`, with no pipelining and no
/// closure requirement anywhere, so nothing specific to this model is involved.
///
/// The trace, with `cargo-like` priorities `1 + transitive reverse-dependents`, so unit 6 has
/// priority 5, unit 8 has 3, units 7 and 9 have 2 and everything else has 1, ties by identifier:
///
///     J = 3: 6,0,1 start at 0. 2 at 2800, 3 at 3900, 8 at 4400, 7 at 6000, 4 at 8400,
///            9 at 9000, 5 at 9300, 10 at 10700, ending at 15600.
///     J = 4: 6,0,1,2 start at 0. 3 at 2800, 4 at 3200, 5 at 3900, 8 at 4400, 7 at 7300,
///            9 at 9000, 10 at 12000, ending at 16900.
///
/// The fourth executor lets the filler units 2, 3, 4 and 5 start earlier, which leaves unit 7 —
/// the one on the long path — waiting behind them until 7300 rather than starting at 6000. Both
/// schedules are legal greedy list schedules under the same priority order.
#[test]
fn i8_another_executor_can_make_the_build_longer() {
    let graph = graham_shape().build();
    let three = simulate(&graph, executors(3), Policy::CargoLike);
    let four = simulate(&graph, executors(4), Policy::CargoLike);

    assert_eq!(three.makespan(), Millis::new(15_600));
    assert_eq!(four.makespan(), Millis::new(16_900));
    assert!(
        four.makespan() > three.makespan(),
        "the recorded executor-count anomaly no longer reproduces"
    );

    // The critical path is what a counterfactual should be anchored on: it does not move.
    let cp = analyze(&graph).critical_path_len();
    assert_eq!(cp, Millis::new(14_200));
    assert!(three.makespan() >= cp && four.makespan() >= cp);
}

/// I8 again, over the corpus. Since the invariant does not hold, what can be asserted is that the
/// anomaly is rare: an extra executor helps far more often than it hurts, and a change that made
/// the simulator gratuitously non-monotone would show up as a jump in the count.
///
/// The exceptions are counted rather than skipped, and the counts are part of the assertion, so
/// this test cannot pass by finding nothing.
#[test]
fn i8_another_executor_almost_always_helps() {
    let mut violations = 0usize;
    let mut improvements = 0usize;
    let mut comparisons = 0usize;
    let mut offenders: BTreeSet<usize> = BTreeSet::new();

    for (index, spec) in corpus().iter().enumerate() {
        let graph = spec.build();
        for policy in POLICIES {
            let mut previous = simulate(&graph, executors(1), policy).makespan();
            for count in 2..=12u32 {
                let makespan = simulate(&graph, executors(count), policy).makespan();
                if makespan > previous {
                    violations += 1;
                    offenders.insert(index);
                } else if makespan < previous {
                    improvements += 1;
                }
                comparisons += 1;
                previous = makespan;
            }
        }
    }

    assert!(comparisons > 3_000, "too few comparisons: {comparisons}");
    assert!(
        improvements > 1_000,
        "an extra executor almost never helped: {improvements}"
    );
    assert!(
        violations * 50 <= comparisons,
        "the executor-count anomaly went from rare to common: {violations} of {comparisons} \
         comparisons, in graphs {offenders:?}"
    );
}

/// Section 7, the anomaly that is deliberately not an invariant. "Removing an edge can increase
/// `makespan(J)`. This is a Graham scheduling anomaly and a genuine property of greedy list
/// scheduling, not a defect. It cannot happen to `CP`, which is monotone under edge removal."
///
/// Five units on two executors. Unit 0 releases everything else; unit 1 needs its whole closure,
/// so it waits for unit 0 to finish outright, while units 2 and 4 are released by unit 0's
/// metadata at 600.
///
///     unit 0: 1000ms, metadata at 600      0 -> 1 metadata, and unit 1 needs its closure
///     unit 1: 1100ms, requires closure     0 -> 2 full
///     unit 2: 1600ms                       0 -> 3 full
///     unit 3: 1200ms                       0 -> 4 metadata
///     unit 4: 1400ms
///
/// With the edge, unit 1 cannot start before unit 0 finishes, and the schedule on two executors
/// is 0 at 0, 4 at 600, 1 at 1000, 2 at 2000, 3 at 2100, ending at 3600. Remove the edge and unit
/// 1 becomes a source: it starts at 0 alongside unit 0, occupying the second executor through the
/// window unit 4 wanted, and the build ends at 3700 instead.
#[test]
fn removing_an_edge_can_lengthen_the_simulated_makespan() {
    let mut spec = GraphSpec {
        units: vec![
            UnitSpec {
                total: 1000,
                metadata_at: Some(600),
                closure: false,
                kind: UnitKind::Lib,
            },
            UnitSpec {
                total: 1100,
                metadata_at: None,
                closure: true,
                kind: UnitKind::Bin,
            },
            UnitSpec {
                total: 1600,
                metadata_at: None,
                closure: false,
                kind: UnitKind::Lib,
            },
            UnitSpec {
                total: 1200,
                metadata_at: None,
                closure: false,
                kind: UnitKind::Lib,
            },
            UnitSpec {
                total: 1400,
                metadata_at: None,
                closure: false,
                kind: UnitKind::Lib,
            },
        ],
        edges: vec![
            (0, 1, EdgeKind::Metadata),
            (0, 2, EdgeKind::Full),
            (0, 3, EdgeKind::Full),
            (0, 4, EdgeKind::Metadata),
        ],
        shape: Shape::Fan,
        weights: Weights::Mixed,
    };

    let graph = spec.build();
    let before = simulate(&graph, executors(2), Policy::CargoLike).makespan();
    assert_eq!(before, Millis::new(3_600));

    let all = ids(&graph);
    let applied = apply(
        &graph,
        &Transform::RemoveEdge {
            from: all[0],
            to: all[1],
        },
    )
    .unwrap();
    let after = simulate(applied.graph(), executors(2), Policy::CargoLike).makespan();
    assert_eq!(after, Millis::new(3_700));
    assert!(
        after > before,
        "the recorded edge-removal anomaly no longer reproduces: {before} became {after}"
    );

    // The critical path moves the other way, which is why section 9.2 anchors the reported saving
    // on it rather than on the simulated makespan.
    let cp_before = analyze(&graph).critical_path_len();
    let cp_after = analyze(applied.graph()).critical_path_len();
    assert!(cp_after <= cp_before, "{cp_before} became {cp_after}");

    // Rebuilding the same shape without the edge, rather than transforming it, gives the same
    // answer: the anomaly is in the schedule, not in the transformation.
    spec.edges.remove(0);
    assert_eq!(
        simulate(&spec.build(), executors(2), Policy::CargoLike).makespan(),
        after
    );
}

/// Weights near the representable maximum, where `Millis` saturates.
///
/// What survives saturation: nothing panics, `CP` and the makespan stay above the bounds of
/// section 6, and the reported slack stays non-negative as I9 requires. What does not survive is
/// the latest-finish time. Once `CP` saturates, the backward pass subtracts from a total that is
/// no longer the sum it stands for, and a unit can be told it must finish before the earliest it
/// can finish. The two-unit graph below is the smallest case: each unit takes half the
/// representable range plus a little, so the chain is one millisecond too long to represent.
#[test]
fn saturating_weights_degrade_without_panicking() {
    let half = 1u64 << 63;
    let spec = GraphSpec {
        units: vec![
            UnitSpec {
                total: half,
                metadata_at: None,
                closure: false,
                kind: UnitKind::Lib,
            },
            UnitSpec {
                total: half,
                metadata_at: None,
                closure: false,
                kind: UnitKind::Lib,
            },
        ],
        edges: vec![(0, 1, EdgeKind::Full)],
        shape: Shape::Chain,
        weights: Weights::Extreme,
    };
    let graph = spec.build();
    let analysis = analyze(&graph);
    let all = ids(&graph);

    // The chain is 2^64 milliseconds long, which does not fit, so it is reported as the largest
    // duration there is.
    assert_eq!(analysis.critical_path_len(), Millis::new(u64::MAX));
    assert_eq!(analysis.earliest_finish(all[0]), Some(Millis::new(half)));

    // I9 holds in the only sense that survives: the reported slack is not negative.
    for &id in &all {
        assert!(analysis.slack(id).unwrap() >= Millis::ZERO);
    }
    // And a deadline is still meetable: the backward pass subtracts from the saturated total, so
    // without a floor at the earliest finish the first unit would be told to finish before it can.
    for &id in &all {
        assert!(
            analysis.latest_finish(id).unwrap() >= analysis.earliest_finish(id).unwrap(),
            "unit {id} is told to finish before the earliest it can"
        );
    }

    // The bounds of sections 5 and 6 still hold, and nothing panics.
    for count in [1u32, 2, 5] {
        let makespan = simulate(&graph, executors(count), Policy::CargoLike).makespan();
        assert!(makespan >= analysis.critical_path_len());
        assert!(makespan >= area_bound(&graph, executors(count)));
    }
}
