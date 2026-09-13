//! Seam properties: the weak handle inside `Waterfall::push_effect`.
//!
//! The disposer a plugin leaves behind when it contributes a layer into a chain
//! it did not create holds a **weak** handle to that chain's late-layer list.
//! That single choice has to survive two opposite orders: a chain that outlives
//! the plugin must actually lose the layer, and a chain that dies first must
//! make the disposer a no-op rather than a panic or a resurrection. A sequence
//! generator is the natural way to interleave them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::Context;

use super::lab::{Chain, LayerProbe, Log, Rng, live_layers, runtime, shrink};

/// How many seeded interleavings the property explores.
const SEQUENCES: u64 = 192;

/// How many chains one sequence contributes into.
const CHAINS: usize = 3;

/// One generated seam operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// Contribute an effect-owned layer into this chain.
    PushOwned(usize),
    /// Contribute a chain-lifetime layer into this chain.
    PushShared(usize),
    /// Drop this chain while the contributing context is still alive.
    DropChain(usize),
}

/// What one chain should hold.
#[derive(Default)]
struct ChainModel {
    /// Every layer id in contribution order.
    pushed: Vec<u32>,
    /// Chain-lifetime layer ids in contribution order.
    shared: Vec<u32>,
    /// Effect-owned layer ids in contribution order.
    owned: Vec<u32>,
}

/// What the generated corpus actually reached.
#[derive(Default)]
struct Coverage {
    owned_into_dropped_chain: usize,
    owned_into_live_chain: usize,
    shared_dropped_with_chain: usize,
}

macro_rules! claim {
    ($condition:expr, $($message:tt)*) => {
        if !$condition {
            return Err(format!($($message)*));
        }
    };
}

/// Run one generated interleaving of contributions, chain drops and shutdown.
fn check(
    runtime: &tokio::runtime::Runtime,
    ops: &[Op],
    coverage: &mut Coverage,
) -> Result<(), String> {
    let dropped = Log::default();
    let mut chains: Vec<Option<Arc<Chain>>> = (0..CHAINS)
        .map(|_| Some(Arc::new(Chain::default())))
        .collect();
    let mut model: Vec<ChainModel> = (0..CHAINS).map(|_| ChainModel::default()).collect();
    let mut context = Context::new();
    let mut owned_in_order: Vec<u32> = Vec::new();
    let mut expected_dropped: Vec<u32> = Vec::new();
    let mut next_id: u32 = 0;

    for op in ops {
        match op {
            Op::PushOwned(slot) => {
                let index = slot % CHAINS;
                let Some(chain) = chains[index].as_ref() else {
                    continue;
                };
                chain.push_effect(&context, LayerProbe::new(next_id, &dropped));
                model[index].pushed.push(next_id);
                model[index].owned.push(next_id);
                owned_in_order.push(next_id);
                next_id += 1;
            }
            Op::PushShared(slot) => {
                let index = slot % CHAINS;
                let Some(chain) = chains[index].as_ref() else {
                    continue;
                };
                chain.push_shared(LayerProbe::new(next_id, &dropped));
                model[index].pushed.push(next_id);
                model[index].shared.push(next_id);
                next_id += 1;
            }
            Op::DropChain(slot) => {
                let index = slot % CHAINS;
                if chains[index].take().is_some() {
                    coverage.owned_into_dropped_chain += model[index].owned.len();
                    coverage.shared_dropped_with_chain += model[index].shared.len();
                    // A chain-lifetime layer has no other owner, so it dies with
                    // the chain. An effect-owned one is still held by the
                    // disposer that has not run yet.
                    expected_dropped.extend(model[index].shared.iter().copied());
                }
            }
        }
        let seen = dropped.entries();
        claim!(
            seen == expected_dropped,
            "before shutdown the withdrawn layers are {seen:?}, expected {expected_dropped:?}"
        );
    }

    for (index, chain) in chains.iter().enumerate() {
        let Some(chain) = chain.as_ref() else {
            continue;
        };
        let running = live_layers(runtime, chain);
        claim!(
            running == model[index].pushed,
            "chain {index} runs {running:?} before shutdown, expected {:?}",
            model[index].pushed
        );
    }

    // The plugin leaves. Every disposer runs, whether or not its chain is
    // still there to be edited.
    context.shutdown();

    expected_dropped.extend(owned_in_order.iter().rev().copied());
    let seen = dropped.entries();
    claim!(
        seen == expected_dropped,
        "after shutdown the withdrawn layers are {seen:?}, expected {expected_dropped:?}"
    );

    for (index, chain) in chains.iter().enumerate() {
        let Some(chain) = chain.as_ref() else {
            continue;
        };
        coverage.owned_into_live_chain += model[index].owned.len();
        let running = live_layers(runtime, chain);
        claim!(
            running == model[index].shared,
            "chain {index} runs {running:?} after shutdown, expected only its \
             chain-lifetime layers {:?}",
            model[index].shared
        );
        claim!(
            chain.shared_layer_count() == model[index].shared.len(),
            "chain {index} counts {} shared layers, expected {}",
            chain.shared_layer_count(),
            model[index].shared.len()
        );
    }
    Ok(())
}

/// One seeded interleaving.
fn generate(rng: &mut Rng) -> Vec<Op> {
    let length = 3 + rng.below(14);
    let mut ops = Vec::with_capacity(length);
    for _ in 0..length {
        ops.push(match rng.below(10) {
            0..=4 => Op::PushOwned(rng.below(CHAINS)),
            5..=7 => Op::PushShared(rng.below(CHAINS)),
            _ => Op::DropChain(rng.below(CHAINS)),
        });
    }
    ops
}

/// Report the shortest interleaving that still disagrees with the model.
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

/// A02 makes seams the plugin extension point, and K09's transaction cannot see
/// a chain that lives behind a type-erased service — which is exactly why the
/// layer carries its own disposer, and why that disposer holds a weak handle.
/// For any generated interleaving of contributions, chain drops and shutdown, a
/// surviving chain loses precisely the effect-owned layers and keeps precisely
/// the chain-lifetime ones, a chain dropped first makes its disposers no-ops
/// rather than panics, and no layer is ever withdrawn twice or resurrected.
#[test]
fn every_generated_seam_interleaving_withdraws_exactly_the_effect_owned_layers() {
    let runtime = runtime();
    let mut coverage = Coverage::default();
    for seed in 1..=SEQUENCES {
        let ops = generate(&mut Rng::seeded(seed));
        if let Err(failure) = check(&runtime, &ops, &mut coverage) {
            panic!("{}", report(&runtime, seed, &ops, &failure));
        }
    }
    assert!(
        coverage.owned_into_dropped_chain >= 50,
        "only {} effect-owned layers outlived their chain; the weak handle is \
         barely exercised",
        coverage.owned_into_dropped_chain
    );
    assert!(
        coverage.owned_into_live_chain >= 50,
        "only {} effect-owned layers were withdrawn from a live chain",
        coverage.owned_into_live_chain
    );
    assert!(
        coverage.shared_dropped_with_chain >= 20,
        "only {} chain-lifetime layers died with their chain",
        coverage.shared_dropped_with_chain
    );
}
