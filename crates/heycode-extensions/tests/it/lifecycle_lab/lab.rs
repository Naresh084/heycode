//! Shared lab machinery: seeded sequences, delete-only shrinking, and the
//! witnesses that make "this registration is still live" observable.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::{Layer, Next, Waterfall};

/// Ceiling on one seam run. A disposer that deadlocks a chain's mutex would
/// otherwise hang the suite rather than fail it.
const SEAM_BUDGET: Duration = Duration::from_secs(5);

/// The seam decision type: layers append their own id, so running a chain
/// returns exactly which layers are live and in what order.
pub type Chain = Waterfall<Vec<u32>>;

/// xorshift64\*, seeded so any failing sequence is reproducible from its seed.
///
/// A framework is not used because the workspace has no property-test
/// dependency and because shrinking a register/dispose sequence needs domain
/// knowledge a generic shrinker does not have — see [`shrink`].
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed`. Every seed produces a distinct stream.
    pub const fn seeded(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..bound`, or `0` for an empty range.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next() % bound as u64) as usize
        }
    }

    /// True with `percent` chance.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// Shrink a failing sequence by deleting from it, largest chunks first.
///
/// Deletion is the only move, and that is a deliberate domain decision: every
/// operation vocabulary in this lab is self-describing and every builder
/// ignores an operation whose context is gone (a registration with no plugin, a
/// release of a handle that was never taken). So *any* subsequence is a
/// runnable sequence, which is what makes plain deletion both valid and enough.
pub fn shrink<Op: Clone>(sequence: &[Op], mut fails: impl FnMut(&[Op]) -> bool) -> Vec<Op> {
    let mut current = sequence.to_vec();
    let mut chunk = current.len().max(1);
    loop {
        let mut index = 0;
        while index < current.len() {
            let mut candidate = current.clone();
            let end = (index + chunk).min(candidate.len());
            candidate.drain(index..end);
            if fails(&candidate) {
                current = candidate;
            } else {
                index += chunk;
            }
        }
        if chunk == 1 {
            return current;
        }
        chunk = (chunk / 2).max(1);
    }
}

/// An append-only ordered log of registration ids, shared with the closures
/// under test.
#[derive(Clone, Default)]
pub struct Log(Arc<Mutex<Vec<u32>>>);

impl Log {
    /// Record `id` at the current end of the log.
    pub fn push(&self, id: u32) {
        self.0.lock().unwrap().push(id);
    }

    /// Everything recorded so far, in order.
    pub fn entries(&self) -> Vec<u32> {
        self.0.lock().unwrap().clone()
    }

    /// Forget everything recorded so far.
    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

/// Moved into every registration; its `Drop` is the moment that registration's
/// own value finally ceased to exist.
///
/// This is what lets one property compare a listener cell, a seam layer and a
/// plain disposer against a single model: a disposer that removed the *wrong*
/// row shows up as the wrong id in this log, and one that removed nothing shows
/// up as a missing id — "no more, no less", made observable.
pub struct DropWitness {
    id: u32,
    log: Log,
}

impl DropWitness {
    /// Witness the lifetime of registration `id`.
    pub fn new(id: u32, log: &Log) -> Self {
        Self {
            id,
            log: log.clone(),
        }
    }
}

impl Drop for DropWitness {
    fn drop(&mut self) {
        self.log.push(self.id);
    }
}

/// The payload the lab emits to enumerate live listeners.
pub struct Ping;

/// A listener that reports its own id when the bus delivers to it.
pub struct ListenerProbe {
    id: u32,
    live: Log,
    _witness: DropWitness,
}

impl ListenerProbe {
    /// A listener for registration `id` reporting into `live`.
    pub fn new(id: u32, live: &Log, dropped: &Log) -> Self {
        Self {
            id,
            live: live.clone(),
            _witness: DropWitness::new(id, dropped),
        }
    }

    /// Report that this listener is still receiving.
    pub fn hit(&self) {
        self.live.push(self.id);
    }
}

/// A seam layer that reports its own id when the chain runs through it.
pub struct LayerProbe {
    id: u32,
    _witness: DropWitness,
}

impl LayerProbe {
    /// A layer for registration `id`.
    pub fn new(id: u32, dropped: &Log) -> Self {
        Self {
            id,
            _witness: DropWitness::new(id, dropped),
        }
    }
}

#[async_trait]
impl Layer<Vec<u32>> for LayerProbe {
    async fn handle(
        &self,
        input: &mut Vec<u32>,
        mut next: Next<'_, Vec<u32>>,
    ) -> anyhow::Result<()> {
        input.push(self.id);
        next.run(input).await
    }
}

/// A current-thread runtime with timers, for driving the async seam.
///
/// The properties themselves stay synchronous so the shrinker can call them as
/// an ordinary predicate; only the seam run needs a runtime.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime builds")
}

/// Run `chain` and return the ids of the layers that are still attached, in
/// execution order. Bounded, so a hung chain fails instead of hanging.
pub fn live_layers(runtime: &tokio::runtime::Runtime, chain: &Chain) -> Vec<u32> {
    runtime.block_on(async {
        let mut ids = Vec::new();
        tokio::time::timeout(SEAM_BUDGET, chain.run(&mut ids))
            .await
            .expect("the seam chain must finish inside its budget")
            .expect("probe layers never fail");
        ids
    })
}
