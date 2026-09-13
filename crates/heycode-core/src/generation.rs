//! K10 plugin reload generations.
//!
//! A reload is not "shut down, then compose again": that order makes a failed
//! reload fatal, because the world you were running is already gone when the
//! replacement turns out not to compose. The order here is the opposite and it
//! is the whole design — compose a candidate, and only a candidate that
//! activated cleanly is allowed to replace the live world.

use std::ops::Deref;
use std::sync::{Arc, Mutex, RwLock};

use crate::{ActivationReport, Context, CoreError, ScopedPlugin};

/// Monotonic identity of one live plugin world.
///
/// Starts at 1 for the initial composition. A failed reload does not consume a
/// number: generations count worlds that actually ran, so "still on 3" is a
/// true statement about what is serving requests, not a gap to explain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(u64);

impl Generation {
    /// The generation of the initial composition.
    pub const FIRST: Self = Self(1);

    /// Sequence number, for display and for ordering two observations.
    #[must_use]
    pub const fn number(self) -> u64 {
        self.0
    }

    const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "generation {}", self.0)
    }
}

/// Why a reload did not happen, with the evidence needed to explain it.
#[derive(Debug)]
#[non_exhaustive]
pub struct ReloadRejected {
    /// The generation still serving requests — unchanged by this attempt.
    pub live: Generation,
    /// Per-plugin outcomes of the candidate that failed to activate.
    pub report: ActivationReport,
    /// The activation error, verbatim.
    pub cause: CoreError,
}

impl std::fmt::Display for ReloadRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "reload rejected, still serving {}: {}",
            self.live, self.cause
        )
    }
}

impl std::error::Error for ReloadRejected {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// What one reload attempt did.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReloadOutcome {
    /// The candidate activated and replaced the live world exactly once.
    Swapped {
        /// The generation that was retired.
        replaced: Generation,
        /// The generation now serving requests.
        live: Generation,
        /// Per-plugin outcomes of the world that is now live.
        report: ActivationReport,
    },
    /// The candidate did not activate; the previous world is untouched.
    Kept(Box<ReloadRejected>),
}

/// Read handle for one exact plugin generation.
///
/// The handle dereferences to its [`Context`], while the allocation remains
/// the terminal lifecycle owner. When the registry and every reader release
/// the generation, its context is shut down before the allocation disappears.
/// This is what lets an in-flight reader outlive the registry without leaking
/// the effects owned by its retired world.
pub struct GenerationContext {
    context: Context,
}

impl GenerationContext {
    fn new(context: Context) -> Self {
        Self { context }
    }
}

impl Deref for GenerationContext {
    type Target = Context;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl Drop for GenerationContext {
    fn drop(&mut self) {
        self.context.shutdown();
    }
}

impl ReloadOutcome {
    /// The generation serving requests after this attempt, either way.
    #[must_use]
    pub const fn live(&self) -> Generation {
        match self {
            Self::Swapped { live, .. } => *live,
            Self::Kept(rejected) => rejected.live,
        }
    }

    /// True only when the live world actually changed.
    #[must_use]
    pub const fn swapped(&self) -> bool {
        matches!(self, Self::Swapped { .. })
    }
}

/// One live plugin world plus the generations that preceded it.
///
/// Readers hold an `Arc` to the world they started with, so a swap never pulls
/// a context out from under work already in flight; the retired world is
/// disposed once its last reader is gone.
pub struct GenerationRegistry {
    live: RwLock<Live>,
    // Retired worlds wait here for their last reader. Disposal is deferred, not
    // skipped: a context dropped without `shutdown` would leak every effect.
    retiring: Mutex<Vec<Arc<GenerationContext>>>,
}

struct Live {
    generation: Generation,
    context: Arc<GenerationContext>,
}

impl GenerationRegistry {
    /// Seed the registry with an already-composed world as [`Generation::FIRST`].
    #[must_use]
    pub fn new(context: Context) -> Self {
        Self {
            live: RwLock::new(Live {
                generation: Generation::FIRST,
                context: Arc::new(GenerationContext::new(context)),
            }),
            retiring: Mutex::new(Vec::new()),
        }
    }

    /// The generation currently serving requests.
    ///
    /// # Panics
    /// Never: a poisoned lock is recovered rather than propagated, because a
    /// panic in an unrelated reader must not take the whole world offline.
    #[must_use]
    pub fn generation(&self) -> Generation {
        match self.live.read() {
            Ok(live) => live.generation,
            Err(poisoned) => poisoned.into_inner().generation,
        }
    }

    /// Borrow the live world. The returned handle keeps that exact generation
    /// alive for as long as it is held, even across a reload.
    #[must_use]
    pub fn context(&self) -> Arc<GenerationContext> {
        match self.live.read() {
            Ok(live) => Arc::clone(&live.context),
            Err(poisoned) => Arc::clone(&poisoned.into_inner().context),
        }
    }

    /// Compose `plugins` and, only if every one activates, replace the live
    /// world with it.
    ///
    /// A rejected candidate is unwound by K09's activation transaction before
    /// this returns, and the live world is never touched — not even briefly.
    pub fn reload(&self, plugins: &[ScopedPlugin]) -> ReloadOutcome {
        // Composed BEFORE any lock is taken and before anything is retired: a
        // candidate that fails must cost the live world nothing at all, not
        // even a moment of unavailability.
        let candidate = crate::compose_scoped_activation(plugins);
        let context = match candidate.context {
            Ok(context) => context,
            Err(cause) => {
                return ReloadOutcome::Kept(Box::new(ReloadRejected {
                    live: self.generation(),
                    report: candidate.report,
                    cause,
                }));
            }
        };

        let mut live = match self.live.write() {
            Ok(live) => live,
            Err(poisoned) => poisoned.into_inner(),
        };
        let replaced = live.generation;
        let retired =
            std::mem::replace(&mut live.context, Arc::new(GenerationContext::new(context)));
        live.generation = replaced.next();
        let now_live = live.generation;
        drop(live);

        self.retire(retired);
        ReloadOutcome::Swapped {
            replaced,
            live: now_live,
            report: candidate.report,
        }
    }

    /// Dispose a retired world once nothing holds it, or park it until then.
    fn retire(&self, retired: Arc<GenerationContext>) {
        let Ok(mut parked) = self.retiring.lock() else {
            return;
        };
        parked.push(retired);
        Self::sweep(&mut parked);
    }

    /// Shut down every parked world whose last reader has gone, and keep the
    /// rest. `Arc::get_mut` succeeding IS the proof that nobody is reading:
    /// disposing on a guess would pull a live context out from under work in
    /// flight, which is the exact failure a generation model exists to prevent.
    fn sweep(parked: &mut Vec<Arc<GenerationContext>>) {
        parked.retain_mut(|context| Arc::get_mut(context).is_none());
    }

    /// Retry disposal of retired worlds and report how many still have readers.
    ///
    /// A reload sweeps on its own, so this is for a host that reloads rarely
    /// and wants retired effects released in between.
    pub fn sweep_retired(&self) -> usize {
        let Ok(mut parked) = self.retiring.lock() else {
            return 0;
        };
        Self::sweep(&mut parked);
        parked.len()
    }

    /// Number of retired worlds still waiting for their last reader.
    ///
    /// Exists so a test can prove a retired world is disposed rather than
    /// leaked, and so an operator can see a reader that never let go.
    #[must_use]
    pub fn pending_disposal(&self) -> usize {
        self.retiring.lock().map(|parked| parked.len()).unwrap_or(0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{CoreResult, Plugin, PluginDescriptor, PluginScope};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MARK: crate::ServiceKey = crate::ServiceKey::new("mark");

    /// Provides a distinguishable value so a test can tell which generation's
    /// context it is holding.
    struct Marks(&'static str, i32);
    impl Plugin for Marks {
        fn name(&self) -> &'static str {
            self.0
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(self.0, "0.0.0", &[crate::PluginContributionKind::Service])
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            ctx.provide(MARK, self.name(), self.1)?;
            Ok(())
        }
    }

    struct Fails(&'static str);
    impl Plugin for Fails {
        fn name(&self) -> &'static str {
            self.0
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(self.0, "0.0.0", &[])
        }
        fn apply(&self, _ctx: &mut Context) -> CoreResult<()> {
            Err(CoreError::other("deliberate"))
        }
    }

    /// Registers a disposer so a test can observe when a retired world is
    /// actually shut down rather than merely dropped.
    struct Disposes(&'static str, Arc<AtomicUsize>);
    impl Plugin for Disposes {
        fn name(&self) -> &'static str {
            self.0
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(self.0, "0.0.0", &[])
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let disposed = Arc::clone(&self.1);
            ctx.effect(move || {
                disposed.fetch_add(1, Ordering::SeqCst);
            });
            Ok(())
        }
    }

    fn scoped(plugin: Box<dyn Plugin>) -> ScopedPlugin {
        ScopedPlugin::new(PluginScope::BuiltIn, plugin)
    }

    fn world(mark: i32) -> Vec<ScopedPlugin> {
        vec![scoped(Box::new(Marks("marks", mark)))]
    }

    fn registry(mark: i32) -> GenerationRegistry {
        let composed = crate::compose_scoped_activation(&world(mark));
        GenerationRegistry::new(composed.context.expect("seed world composes"))
    }

    #[test]
    fn a_successful_reload_swaps_exactly_once() {
        let registry = registry(1);
        assert_eq!(registry.generation(), Generation::FIRST);
        assert_eq!(*registry.context().get::<i32>(MARK).unwrap(), 1);

        let outcome = registry.reload(&world(2));

        let ReloadOutcome::Swapped {
            replaced,
            live,
            report,
        } = outcome
        else {
            panic!("a clean candidate must swap");
        };
        assert_eq!(replaced, Generation::FIRST);
        assert_eq!(live.number(), 2, "exactly one increment, not two");
        assert!(report.healthy());
        assert_eq!(registry.generation().number(), 2);
        assert_eq!(
            *registry.context().get::<i32>(MARK).unwrap(),
            2,
            "the live world must be the candidate's, not the old one"
        );
    }

    #[test]
    fn a_failed_reload_keeps_the_last_good_world() {
        let registry = registry(1);
        let before = registry.generation();

        let outcome = registry.reload(&[
            scoped(Box::new(Marks("marks", 2))),
            scoped(Box::new(Fails("culprit"))),
        ]);

        assert!(!outcome.swapped());
        assert_eq!(outcome.live(), before, "the live generation must not move");
        assert_eq!(registry.generation(), before);
        assert_eq!(
            *registry.context().get::<i32>(MARK).unwrap(),
            1,
            "the last good world must still be serving, with its own value"
        );
    }

    #[test]
    fn a_rejected_candidate_names_the_plugin_and_what_still_serves() {
        let registry = registry(1);
        let outcome = registry.reload(&[
            scoped(Box::new(Marks("marks", 2))),
            scoped(Box::new(Fails("culprit"))),
            scoped(Box::new(Marks("never-ran", 3))),
        ]);

        let ReloadOutcome::Kept(rejected) = outcome else {
            panic!("a failing candidate must be rejected");
        };
        assert_eq!(rejected.live, Generation::FIRST);
        assert!(rejected.cause.to_string().contains("deliberate"));
        let failed = rejected.report.failure().expect("a failed row exists");
        assert_eq!(failed.plugin, "culprit");
        assert!(matches!(
            rejected.report.outcome("never-ran"),
            Some(crate::PluginActivationOutcome::NotAttempted)
        ));
        assert!(rejected.to_string().contains("generation 1"));
    }

    #[test]
    fn generations_count_worlds_that_ran_not_reload_attempts() {
        let registry = registry(1);
        for _ in 0..3 {
            assert!(
                !registry
                    .reload(&[scoped(Box::new(Fails("nope")))])
                    .swapped()
            );
        }
        assert_eq!(
            registry.generation(),
            Generation::FIRST,
            "three failures must not consume three numbers"
        );
        assert!(registry.reload(&world(2)).swapped());
        assert_eq!(registry.generation().number(), 2);
    }

    #[test]
    fn a_reader_holding_a_generation_keeps_it_alive_across_a_swap() {
        let disposed = Arc::new(AtomicUsize::new(0));
        let seed = crate::compose_scoped_activation(&[scoped(Box::new(Disposes(
            "disposes",
            Arc::clone(&disposed),
        )))]);
        let registry = GenerationRegistry::new(seed.context.expect("seed composes"));

        // Work in flight holds the world it started with.
        let in_flight = registry.context();
        assert!(registry.reload(&world(2)).swapped());

        assert_eq!(
            disposed.load(Ordering::SeqCst),
            0,
            "a retired world must not be disposed while it is still being read"
        );
        assert_eq!(registry.pending_disposal(), 1);
        assert!(!in_flight.is_closed(), "the reader's world is still usable");

        // The reader finishes; the retired world is released on the next sweep.
        drop(in_flight);
        assert_eq!(registry.sweep_retired(), 0);
        assert_eq!(
            disposed.load(Ordering::SeqCst),
            1,
            "a released world must be shut down exactly once"
        );
    }

    #[test]
    fn a_rejected_reload_retires_nothing() {
        let disposed = Arc::new(AtomicUsize::new(0));
        let seed = crate::compose_scoped_activation(&[scoped(Box::new(Disposes(
            "disposes",
            Arc::clone(&disposed),
        )))]);
        let registry = GenerationRegistry::new(seed.context.expect("seed composes"));

        assert!(
            !registry
                .reload(&[scoped(Box::new(Fails("nope")))])
                .swapped()
        );

        assert_eq!(registry.pending_disposal(), 0, "nothing was retired");
        assert_eq!(
            disposed.load(Ordering::SeqCst),
            0,
            "the live world's effects must survive a failed reload"
        );
    }
}
