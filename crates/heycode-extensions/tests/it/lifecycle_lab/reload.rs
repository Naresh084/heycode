//! Reload properties: K10's generation model under generated reload orders.
//!
//! The three claims under test are the ones the ordering in
//! `GenerationRegistry::reload` exists to make true — a candidate is composed
//! before any lock is taken, so a rejected candidate costs the live world
//! nothing; a retired world is disposed only once `Arc::get_mut` proves nobody
//! is reading it; and a rejected candidate withdraws everything it registered
//! into a seam it did not create.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, GenerationContext, GenerationRegistry, Plugin,
    PluginActivationOutcome, PluginContributionKind, PluginDescriptor, PluginScope, ReloadOutcome,
    ScopedPlugin, ServiceKey, compose_scoped_activation,
};

use super::lab::{Chain, LayerProbe, Log, Rng, live_layers, runtime, shrink};

/// How many seeded reload sequences the property explores.
const SEQUENCES: u64 = 192;

/// The value that says which world a handle is actually reading.
const MARK: ServiceKey = ServiceKey::new("lab/mark");

/// One generated reload operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// Reload with a candidate that activates cleanly.
    ReloadClean,
    /// Reload with a candidate whose second plugin refuses to activate.
    ReloadRejected,
    /// Take and keep a handle on the world currently serving.
    Hold,
    /// Release the handle in this slot, if there is still one there.
    Release(usize),
    /// Ask the registry to retry disposal of retired worlds.
    Sweep,
}

/// The plugin every candidate world is built from: it publishes the mark that
/// identifies its world, contributes a layer into a seam it does not own, and
/// registers a disposer that reports the world's shutdown.
struct GenerationPlugin {
    mark: u32,
    chain: Arc<Chain>,
    dropped: Log,
    disposed: Log,
}

impl Plugin for GenerationPlugin {
    fn name(&self) -> &'static str {
        "lab-generation"
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            "lab-generation",
            "0.0.0",
            &[PluginContributionKind::Service],
        )
    }

    fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
        ctx.provide(MARK, self.name(), self.mark)?;
        self.chain
            .push_effect(ctx, LayerProbe::new(self.mark, &self.dropped));
        let disposed = self.disposed.clone();
        let mark = self.mark;
        ctx.effect(move || disposed.push(mark));
        Ok(())
    }
}

/// The second plugin of a candidate that must not be allowed to go live.
struct RefusingPlugin;

impl Plugin for RefusingPlugin {
    fn name(&self) -> &'static str {
        "lab-refuses"
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in("lab-refuses", "0.0.0", &[])
    }

    fn apply(&self, _ctx: &mut Context) -> CoreResult<()> {
        Err(CoreError::other("candidate refuses to activate"))
    }
}

/// Everything shared across generations: the seam they all contribute into and
/// the logs that outlive every world.
struct Lab {
    chain: Arc<Chain>,
    dropped: Log,
    disposed: Log,
}

impl Lab {
    fn new() -> Self {
        Self {
            chain: Arc::new(Chain::default()),
            dropped: Log::default(),
            disposed: Log::default(),
        }
    }

    fn world(&self, mark: u32, reject: bool) -> Vec<ScopedPlugin> {
        let mut plugins = vec![ScopedPlugin::new(
            PluginScope::BuiltIn,
            Box::new(GenerationPlugin {
                mark,
                chain: Arc::clone(&self.chain),
                dropped: self.dropped.clone(),
                disposed: self.disposed.clone(),
            }) as Box<dyn Plugin>,
        )];
        if reject {
            plugins.push(ScopedPlugin::new(
                PluginScope::BuiltIn,
                Box::new(RefusingPlugin) as Box<dyn Plugin>,
            ));
        }
        plugins
    }
}

/// What the registry should look like, tracked independently of it.
struct Model {
    live_generation: u64,
    live_mark: u32,
    /// Retired worlds still waiting for a reader, in retirement order.
    parked: Vec<(u64, u32)>,
    /// Outstanding handles per generation.
    held: BTreeMap<u64, usize>,
    /// Marks of worlds shut down, in disposal order.
    disposed: Vec<u32>,
    /// Marks whose seam layer is still attached, in contribution order.
    layers: Vec<u32>,
}

impl Model {
    /// `Arc::get_mut` succeeding is the registry's proof that nobody is
    /// reading; here the equivalent proof is an outstanding-handle count of
    /// zero, derived from the operation sequence rather than from the registry.
    fn sweep(&mut self, coverage: &mut Coverage) {
        for (generation, mark) in std::mem::take(&mut self.parked) {
            if self.held.get(&generation).copied().unwrap_or(0) > 0 {
                coverage.deferred_disposals += 1;
                self.parked.push((generation, mark));
            } else {
                self.disposed.push(mark);
                self.layers.retain(|live| *live != mark);
            }
        }
    }
}

/// What the generated corpus actually reached, so the property cannot silently
/// stop exercising the case that matters.
#[derive(Default)]
struct Coverage {
    clean_reloads: usize,
    rejected_reloads: usize,
    reloads_with_readers: usize,
    deferred_disposals: usize,
    releases: usize,
    readers_at_registry_drop: usize,
}

macro_rules! claim {
    ($condition:expr, $($message:tt)*) => {
        if !$condition {
            return Err(format!($($message)*));
        }
    };
}

type Handle = Option<(u64, u32, Arc<GenerationContext>)>;

/// Compare every observable of the registry against the model.
fn verify(
    runtime: &tokio::runtime::Runtime,
    lab: &Lab,
    registry: &GenerationRegistry,
    model: &Model,
    handles: &[Handle],
) -> Result<(), String> {
    claim!(
        registry.generation().number() == model.live_generation,
        "generation is {}, expected {}",
        registry.generation().number(),
        model.live_generation
    );
    {
        let live = registry.context();
        // Deliberately not `unwrap`: a mutation that swaps an unrelated context
        // into the live slot must produce a readable disagreement, not a panic
        // from inside the checker.
        claim!(
            live.get::<u32>(MARK).as_deref() == Some(&model.live_mark),
            "the live world reads mark {:?}, expected {}",
            live.get::<u32>(MARK).as_deref(),
            model.live_mark
        );
        claim!(!live.is_closed(), "the live world must never be closed");
    }
    claim!(
        registry.pending_disposal() == model.parked.len(),
        "{} worlds await disposal, expected {}",
        registry.pending_disposal(),
        model.parked.len()
    );
    let disposed = lab.disposed.entries();
    claim!(
        disposed == model.disposed,
        "worlds shut down are {disposed:?}, expected {:?}",
        model.disposed
    );
    let dropped = lab.dropped.entries();
    claim!(
        dropped == model.disposed,
        "a world's seam layer must be withdrawn exactly when the world is \
         disposed: withdrawn {dropped:?}, disposed {:?}",
        model.disposed
    );
    let layers = live_layers(runtime, &lab.chain);
    claim!(
        layers == model.layers,
        "the seam still runs {layers:?}, expected {:?}",
        model.layers
    );
    for (generation, mark, context) in handles.iter().flatten() {
        claim!(
            !context.is_closed(),
            "generation {generation} was disposed while a reader still held it"
        );
        claim!(
            context.get::<u32>(MARK).as_deref() == Some(mark),
            "a held handle must keep reading its own generation, read {:?} not {mark}",
            context.get::<u32>(MARK).as_deref()
        );
    }
    Ok(())
}

/// Run one generated reload sequence against the registry and the model.
fn check(
    runtime: &tokio::runtime::Runtime,
    ops: &[Op],
    coverage: &mut Coverage,
) -> Result<(), String> {
    let lab = Lab::new();
    let seed = compose_scoped_activation(&lab.world(1, false));
    let Ok(context) = seed.context else {
        return Err("the seed world must compose".to_owned());
    };
    let registry = GenerationRegistry::new(context);
    let mut model = Model {
        live_generation: 1,
        live_mark: 1,
        parked: Vec::new(),
        held: BTreeMap::new(),
        disposed: Vec::new(),
        layers: vec![1],
    };
    let mut handles: Vec<Handle> = Vec::new();
    let mut next_mark: u32 = 2;

    verify(runtime, &lab, &registry, &model, &handles)?;

    for op in ops {
        match op {
            Op::ReloadClean => {
                coverage.clean_reloads += 1;
                let mark = next_mark;
                next_mark += 1;
                let outcome = registry.reload(&lab.world(mark, false));
                let ReloadOutcome::Swapped {
                    replaced,
                    live,
                    report,
                } = &outcome
                else {
                    return Err("a candidate that activates cleanly must swap".to_owned());
                };
                claim!(
                    replaced.number() == model.live_generation,
                    "the retired generation is {}, expected {}",
                    replaced.number(),
                    model.live_generation
                );
                claim!(
                    live.number() == model.live_generation + 1,
                    "a swap must advance exactly one generation, went to {}",
                    live.number()
                );
                claim!(report.healthy(), "a swapped world must report healthy");

                if model.held.get(&model.live_generation).copied().unwrap_or(0) > 0 {
                    coverage.reloads_with_readers += 1;
                }
                model.parked.push((model.live_generation, model.live_mark));
                model.live_generation += 1;
                model.live_mark = mark;
                model.layers.push(mark);
                model.sweep(coverage);
            }
            Op::ReloadRejected => {
                coverage.rejected_reloads += 1;
                let mark = next_mark;
                next_mark += 1;
                let before = {
                    let live = registry.context();
                    Arc::as_ptr(&live)
                };
                let outcome = registry.reload(&lab.world(mark, true));
                let ReloadOutcome::Kept(rejected) = &outcome else {
                    return Err("a candidate that refuses must be kept out".to_owned());
                };
                claim!(
                    rejected.live.number() == model.live_generation,
                    "a rejected reload reported {} as live, expected {}",
                    rejected.live.number(),
                    model.live_generation
                );
                let failure = rejected
                    .report
                    .failure()
                    .ok_or_else(|| "a rejected candidate must name its culprit".to_owned())?;
                claim!(
                    failure.plugin == "lab-refuses",
                    "the culprit is `{}`, expected `lab-refuses`",
                    failure.plugin
                );
                claim!(
                    matches!(
                        rejected.report.outcome("lab-generation"),
                        Some(PluginActivationOutcome::Activated)
                    ),
                    "the candidate's first plugin did activate before the refusal"
                );
                let after = {
                    let live = registry.context();
                    Arc::as_ptr(&live)
                };
                claim!(
                    std::ptr::eq(before, after),
                    "a rejected reload replaced the live context anyway"
                );
                // The candidate ran far enough to contribute into a seam it did
                // not create; unwinding must take that back with it.
                model.disposed.push(mark);
            }
            Op::Hold => {
                handles.push(Some((
                    model.live_generation,
                    model.live_mark,
                    registry.context(),
                )));
                *model.held.entry(model.live_generation).or_insert(0) += 1;
            }
            Op::Release(slot) => {
                let index = slot % handles.len().max(1);
                if let Some((generation, _, context)) =
                    handles.get_mut(index).and_then(Option::take)
                {
                    drop(context);
                    coverage.releases += 1;
                    if let Some(count) = model.held.get_mut(&generation) {
                        *count -= 1;
                    }
                }
            }
            Op::Sweep => {
                model.sweep(coverage);
                let pending = registry.sweep_retired();
                claim!(
                    pending == model.parked.len(),
                    "the sweep left {pending} worlds parked, expected {}",
                    model.parked.len()
                );
            }
        }
        verify(runtime, &lab, &registry, &model, &handles)?;
    }

    // Terminal ownership is part of the generation contract too. The host may
    // disappear while readers still hold retired worlds, so dropping the
    // registry must release worlds with no readers and transfer final teardown
    // to the last surviving reader. Once those readers leave, every world that
    // ever activated (including rejected candidates) must have disposed
    // exactly once and withdrawn its seam layer.
    coverage.readers_at_registry_drop += handles.iter().flatten().count();
    drop(registry);
    for (generation, mark, context) in handles.iter().flatten() {
        claim!(
            !context.is_closed(),
            "generation {generation} was disposed when its registry left, before its reader"
        );
        claim!(
            context.get::<u32>(MARK).as_deref() == Some(mark),
            "generation {generation} stopped serving its reader during registry teardown"
        );
    }
    drop(handles);

    let expected: Vec<u32> = (1..next_mark).collect();
    let mut disposed = lab.disposed.entries();
    disposed.sort_unstable();
    claim!(
        disposed == expected,
        "terminal teardown disposed worlds {disposed:?}, expected every activated world {expected:?}"
    );
    let mut dropped = lab.dropped.entries();
    dropped.sort_unstable();
    claim!(
        dropped == expected,
        "terminal teardown withdrew layers {dropped:?}, expected every activated world {expected:?}"
    );
    claim!(
        live_layers(runtime, &lab.chain).is_empty(),
        "a generation seam layer survived its last reader"
    );
    Ok(())
}

/// One seeded reload sequence.
fn generate(rng: &mut Rng) -> Vec<Op> {
    let length = 4 + rng.below(16);
    let mut ops = Vec::with_capacity(length);
    for _ in 0..length {
        ops.push(match rng.below(10) {
            0..=2 => Op::ReloadClean,
            3 | 4 => Op::ReloadRejected,
            5 | 6 => Op::Hold,
            7 | 8 => Op::Release(rng.below(6)),
            _ => Op::Sweep,
        });
    }
    ops
}

/// Report the shortest sequence that still disagrees with the model.
fn report(runtime: &tokio::runtime::Runtime, seed: u64, ops: &[Op], failure: &str) -> String {
    let minimal = shrink(ops, |candidate| {
        check(runtime, candidate, &mut Coverage::default()).is_err()
    });
    let detail = check(runtime, &minimal, &mut Coverage::default())
        .err()
        .unwrap_or_else(|| failure.to_owned());
    format!(
        "seed {seed}: {detail}\n  minimal sequence ({} ops): {minimal:?}\n  original ({} ops)",
        minimal.len(),
        ops.len()
    )
}

/// K10's reload order — compose the candidate before taking any lock, retire
/// nothing until it activated, dispose only when `Arc::get_mut` proves the last
/// reader is gone — is a claim about arbitrary interleavings of reloads,
/// readers and sweeps. This generates them: for any such interleaving the live
/// generation, the live value, the parked set, the disposal order and the seam
/// contributions of every generation match a model derived from the sequence
/// alone, and no handle ever finds its world closed underneath it.
#[test]
fn every_generated_reload_order_keeps_the_registry_exactly_where_the_model_says() {
    let runtime = runtime();
    let mut coverage = Coverage::default();
    for seed in 1..=SEQUENCES {
        let ops = generate(&mut Rng::seeded(seed));
        if let Err(failure) = check(&runtime, &ops, &mut coverage) {
            panic!("{}", report(&runtime, seed, &ops, &failure));
        }
    }
    assert!(
        coverage.clean_reloads >= 100,
        "only {} clean reloads across {SEQUENCES} sequences",
        coverage.clean_reloads
    );
    assert!(
        coverage.rejected_reloads >= 100,
        "only {} rejected reloads across {SEQUENCES} sequences",
        coverage.rejected_reloads
    );
    assert!(
        coverage.releases >= 50,
        "only {} handle releases across {SEQUENCES} sequences",
        coverage.releases
    );
    assert!(
        coverage.reloads_with_readers >= 20,
        "only {} reloads retired a world that still had a reader; the invariant \
         this lab exists for is barely exercised",
        coverage.reloads_with_readers
    );
    assert!(
        coverage.deferred_disposals >= 20,
        "only {} sweeps left a world parked for its reader",
        coverage.deferred_disposals
    );
    assert!(
        coverage.readers_at_registry_drop >= 50,
        "only {} readers outlived their generation registry; last-reader teardown is barely exercised",
        coverage.readers_at_registry_drop
    );
}
