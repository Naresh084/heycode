//! Typed event bus and waterfall interception seams.
//!
//! Two mechanisms with different jobs:
//! - [`EventBus`]: typed fire-and-forget notification (`emit`). Listener panics
//!   are contained so one bad listener cannot starve the rest.
//! - [`Waterfall<T>`]: ordered around-middleware for interception decisions.
//!   A layer calls `next.run(input)?` to delegate; returning without calling it
//!   short-circuits the chain deliberately.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;

/// One registered listener cell, type-erased behind `Any`.
type ListenerCellArc = Arc<dyn Any + Send + Sync>;
/// Listener registry keyed by payload [`TypeId`].
type ListenerMap = Arc<RwLock<HashMap<TypeId, Vec<ListenerCellArc>>>>;

/// Typed fire-and-forget notification bus. Cheap to clone.
#[derive(Clone, Default)]
pub struct EventBus {
    listeners: ListenerMap,
}

struct ListenerCell<E> {
    f: Arc<dyn Fn(&E) + Send + Sync>,
}

impl EventBus {
    /// Whether two handles publish to the same listener registry.
    #[must_use]
    pub fn same_bus(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.listeners, &other.listeners)
    }

    /// Register a synchronous listener for payload type `E`.
    pub fn on<E: Send + Sync + 'static>(&self, f: impl Fn(&E) + Send + Sync + 'static) {
        let _ = self.insert_listener(f);
    }

    /// Register a synchronous listener owned by one plugin context effect.
    /// Rollback/shutdown removes only this exact listener cell.
    pub fn on_effect<E: Send + Sync + 'static>(
        &self,
        context: &crate::Context,
        f: impl Fn(&E) + Send + Sync + 'static,
    ) {
        let Some(cell) = self.insert_listener(f) else {
            return;
        };
        let listeners = Arc::downgrade(&self.listeners);
        context.effect(move || {
            let Some(listeners) = listeners.upgrade() else {
                return;
            };
            let Ok(mut map) = listeners.write() else {
                return;
            };
            let type_id = TypeId::of::<E>();
            let Some(cells) = map.get_mut(&type_id) else {
                return;
            };
            cells.retain(|registered| !Arc::ptr_eq(registered, &cell));
            if cells.is_empty() {
                map.remove(&type_id);
            }
        });
    }

    /// Total registered listener cells across every payload type.
    ///
    /// Activation verification uses this to detect a listener a failed plugin
    /// registered without an owning effect, which no rollback can remove.
    pub(crate) fn listener_count(&self) -> usize {
        self.listeners
            .read()
            .map_or(0, |map| map.values().map(Vec::len).sum())
    }

    fn insert_listener<E: Send + Sync + 'static>(
        &self,
        f: impl Fn(&E) + Send + Sync + 'static,
    ) -> Option<ListenerCellArc> {
        let cell: ListenerCellArc = Arc::new(ListenerCell { f: Arc::new(f) });
        if let Ok(mut map) = self.listeners.write() {
            map.entry(TypeId::of::<E>()).or_default().push(cell.clone());
            Some(cell)
        } else {
            None
        }
    }

    /// Notify every listener for `E`. Panicking listeners are contained;
    /// remaining listeners still run.
    pub fn emit<E: Send + Sync + 'static>(&self, event: E) {
        let event = &event;
        let cells = {
            match self.listeners.read() {
                Ok(map) => map.get(&TypeId::of::<E>()).cloned().unwrap_or_default(),
                Err(_) => return,
            }
        };
        for cell in cells {
            if let Some(listener) = cell.downcast_ref::<ListenerCell<E>>() {
                // A panicking observer must not take down the host or starve peers.
                let _ = catch_unwind(AssertUnwindSafe(|| (listener.f)(event)));
            }
        }
    }
}

/// One middleware layer of a [`Waterfall`] seam. Async so layers may await
/// human decisions (e.g. approval dialogs) mid-chain.
#[async_trait]
pub trait Layer<T>: Send + Sync {
    /// Inspect/mutate `input`, then either delegate via `next.run(input).await?`
    /// or short-circuit by returning without calling `next`.
    ///
    /// # Errors
    /// Propagated verbatim to the seam caller.
    async fn handle(&self, input: &mut T, next: Next<'_, T>) -> anyhow::Result<()>;
}

/// Whether an around-middleware chain reached its terminal delegate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaterfallCompletion {
    /// Every participating layer delegated and the end of the chain ran.
    Completed,
    /// A layer returned without delegating to the end of the chain.
    ShortCircuited,
}

/// The remainder of a waterfall chain, handed to each [`Layer`].
pub struct Next<'a, T> {
    layers: &'a [Arc<dyn Layer<T>>],
    index: usize,
    completed: &'a AtomicBool,
}

impl<T: Send> Next<'_, T> {
    /// Delegate to the next layer (or finish the chain).
    ///
    /// # Errors
    /// Propagates whatever deeper layers return.
    pub async fn run(&mut self, input: &mut T) -> anyhow::Result<()> {
        if let Some(layer) = self.layers.get(self.index) {
            self.index += 1;
            layer
                .handle(
                    input,
                    Next {
                        layers: self.layers,
                        index: self.index,
                        completed: self.completed,
                    },
                )
                .await
        } else {
            self.completed.store(true, AtomicOrdering::Release);
            Ok(())
        }
    }
}

/// An ordered middleware chain over one decision type `T`.
///
/// Early layers are registered at build time (`push`); later owners (plugins
/// that receive the published chain as an immutable service) append through
/// [`Self::push_shared`]. Execution order: early layers, then shared layers
/// in their own registration order.
pub struct Waterfall<T> {
    layers: Vec<Arc<dyn Layer<T>>>,
    // `Arc` so `push_effect` can hand its disposer a weak handle: a disposer
    // that runs after the chain is dropped must be a no-op, not a resurrection.
    late: Arc<Mutex<Vec<Arc<dyn Layer<T>>>>>,
}

impl<T> Default for Waterfall<T> {
    fn default() -> Self {
        Self {
            layers: Vec::new(),
            late: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl<T: Send> Waterfall<T> {
    /// An empty chain that passes input through unchanged.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a layer. Order of registration = order of execution.
    pub fn push(&mut self, layer: impl Layer<T> + 'static) {
        self.layers.push(Arc::new(layer));
    }

    /// Append a layer AFTER publication (shared/interior-mutable path);
    /// shared layers run after all early layers in their own order.
    ///
    /// Chain-lifetime only. A plugin-owned layer must use
    /// [`Self::push_effect`] instead: a layer pushed here survives a failed
    /// activation, and K09's transaction cannot see it because the chain lives
    /// behind a service rather than in the `Context`.
    pub fn push_shared(&self, layer: impl Layer<T> + 'static) {
        if let Ok(mut late) = self.late.lock() {
            late.push(Arc::new(layer));
        }
    }

    /// Append a shared layer owned by one plugin context effect.
    ///
    /// Rollback or shutdown removes this exact layer and no other, which is
    /// what makes a plugin-contributed seam layer part of the activation
    /// transaction. This is the `Waterfall` counterpart of
    /// [`EventBus::on_effect`], and the same rule applies: `push_shared` is for
    /// the chain's own owner, `push_effect` for anyone contributing into a
    /// chain they did not create.
    pub fn push_effect(&self, context: &crate::Context, layer: impl Layer<T> + 'static)
    where
        T: 'static,
    {
        let layer: Arc<dyn Layer<T>> = Arc::new(layer);
        {
            let Ok(mut late) = self.late.lock() else {
                return;
            };
            late.push(Arc::clone(&layer));
        }
        // Weak, so a disposer running after the chain is gone is a no-op rather
        // than keeping the chain alive for the life of the effect.
        let late = Arc::downgrade(&self.late);
        context.effect(move || {
            let Some(late) = late.upgrade() else {
                return;
            };
            let Ok(mut layers) = late.lock() else {
                return;
            };
            layers.retain(|registered| !Arc::ptr_eq(registered, &layer));
        });
    }

    /// Number of shared layers currently appended.
    ///
    /// For a chain owner auditing its own seam. K09's activation fingerprint
    /// cannot call this: a chain lives behind a type-erased service, so no
    /// generic sweep can reach it. That is precisely why a plugin-owned layer
    /// must carry its own disposer via [`Self::push_effect`].
    #[must_use]
    pub fn shared_layer_count(&self) -> usize {
        self.late.lock().map(|late| late.len()).unwrap_or(0)
    }

    /// Run the chain over `input`: early layers then shared layers.
    ///
    /// # Errors
    /// Propagates the first error any layer returns.
    pub async fn run(&self, input: &mut T) -> anyhow::Result<()> {
        self.run_checked(input).await.map(|_| ())
    }

    /// Run the chain and report whether every layer delegated to its end.
    ///
    /// This lets policy callers distinguish an intentional, typed refusal
    /// from a layer that accidentally forgot `next`. Ordinary notification
    /// seams may continue using [`Self::run`] when the distinction is not part
    /// of their contract.
    ///
    /// # Errors
    /// Propagates the first error any layer returns.
    pub async fn run_checked(&self, input: &mut T) -> anyhow::Result<WaterfallCompletion> {
        let mut all: Vec<Arc<dyn Layer<T>>> = self.layers.clone();
        if let Ok(late) = self.late.lock() {
            all.extend(late.iter().cloned());
        }
        let completed = AtomicBool::new(false);
        let mut next = Next {
            layers: &all,
            index: 0,
            completed: &completed,
        };
        next.run(input).await?;
        Ok(if completed.load(AtomicOrdering::Acquire) {
            WaterfallCompletion::Completed
        } else {
            WaterfallCompletion::ShortCircuited
        })
    }

    /// Number of registered layers (early + shared; tests/diagnostics).
    #[must_use]
    pub fn len(&self) -> usize {
        let late = self.late.lock().map(|l| l.len()).unwrap_or(0);
        self.layers.len() + late
    }

    /// True when no layers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AddOne;
    #[async_trait]
    impl Layer<i32> for AddOne {
        async fn handle(&self, input: &mut i32, mut next: Next<'_, i32>) -> anyhow::Result<()> {
            *input += 1;
            next.run(input).await
        }
    }

    struct DoubleIfPositive;
    #[async_trait]
    impl Layer<i32> for DoubleIfPositive {
        async fn handle(&self, input: &mut i32, _next: Next<'_, i32>) -> anyhow::Result<()> {
            if *input > 0 {
                *input *= 2;
            }
            Ok(()) // deliberate short-circuit: never delegates
        }
    }

    /// A02 makes seams the plugin extension point; K09 makes activation a
    /// transaction. This is the join: a layer a plugin contributes into a chain
    /// it did not create must leave with that plugin, or a failed activation
    /// silently keeps mutating every later request through the seam.
    #[tokio::test]
    async fn an_effect_owned_layer_leaves_with_its_plugin_and_takes_nothing_else() {
        let chain: Waterfall<i32> = Waterfall::default();

        // The chain's own owner pushes a lifetime layer; a plugin contributes
        // one through an effect. Both run.
        chain.push_shared(AddOne);
        let mut context = crate::Context::default();
        chain.push_effect(&context, AddOne);
        assert_eq!(chain.shared_layer_count(), 2);
        let mut value = 0;
        chain.run(&mut value).await.expect("chain runs");
        assert_eq!(value, 2, "both layers must run before rollback");

        // The plugin's activation fails and unwinds.
        context.shutdown();

        assert_eq!(
            chain.shared_layer_count(),
            1,
            "rollback must remove the effect-owned layer and keep the owner's"
        );
        let mut value = 0;
        chain.run(&mut value).await.expect("chain still runs");
        assert_eq!(value, 1, "the withdrawn layer must no longer observe input");
    }

    /// The disposer holds a weak handle, so a rollback that happens after the
    /// seam is gone is a no-op rather than a panic or a resurrection.
    #[tokio::test]
    async fn disposing_an_effect_owned_layer_after_its_chain_is_dropped_is_a_no_op() {
        let mut context = crate::Context::default();
        {
            let chain: Waterfall<i32> = Waterfall::default();
            chain.push_effect(&context, AddOne);
            assert_eq!(chain.shared_layer_count(), 1);
        }
        context.shutdown(); // must not panic
    }

    struct Boom;
    #[async_trait]
    impl Layer<i32> for Boom {
        async fn handle(&self, _input: &mut i32, mut next: Next<'_, i32>) -> anyhow::Result<()> {
            next.run(_input).await?;
            Err(anyhow::anyhow!("boom"))
        }
    }

    #[tokio::test]
    async fn waterfall_runs_layers_in_order_and_delegates() {
        let mut wf = Waterfall::new();
        wf.push(AddOne);
        wf.push(AddOne);
        let mut value = 0;
        wf.run(&mut value).await.unwrap();
        assert_eq!(value, 2);
    }

    #[tokio::test]
    async fn waterfall_short_circuit_skips_downstream() {
        let mut wf = Waterfall::new();
        wf.push(DoubleIfPositive);
        wf.push(AddOne);
        let mut value = 3;
        wf.run(&mut value).await.unwrap();
        assert_eq!(value, 6); // AddOne never ran
    }

    #[tokio::test]
    async fn empty_waterfall_is_identity() {
        let wf: Waterfall<String> = Waterfall::new();
        assert!(wf.is_empty());
        let mut s = String::from("x");
        wf.run(&mut s).await.unwrap();
        assert_eq!(s, "x");
    }

    #[tokio::test]
    async fn waterfall_propagates_layer_errors() {
        let mut wf = Waterfall::new();
        wf.push(Boom);
        let mut v = 1;
        assert!(wf.run(&mut v).await.is_err());
    }

    #[tokio::test]
    async fn checked_waterfall_distinguishes_completion_from_short_circuit() {
        struct Stop;
        #[async_trait]
        impl Layer<i32> for Stop {
            async fn handle(&self, input: &mut i32, _next: Next<'_, i32>) -> anyhow::Result<()> {
                *input += 10;
                Ok(())
            }
        }

        let mut completed = Waterfall::new();
        completed.push(AddOne);
        let mut completed_value = 0;
        assert_eq!(
            completed.run_checked(&mut completed_value).await.unwrap(),
            WaterfallCompletion::Completed
        );
        assert_eq!(completed_value, 1);

        let mut stopped = Waterfall::new();
        stopped.push(Stop);
        stopped.push(AddOne);
        let mut stopped_value = 0;
        assert_eq!(
            stopped.run_checked(&mut stopped_value).await.unwrap(),
            WaterfallCompletion::ShortCircuited
        );
        assert_eq!(stopped_value, 10);
    }

    #[test]
    fn event_bus_delivers_to_all_listeners_of_a_type() {
        let bus = EventBus::default();
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        bus.on::<String>(move |_| {
            h2.fetch_add(1, Ordering::SeqCst);
        });
        bus.emit("hello".to_owned());
        bus.emit("world".to_owned());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn event_listener_effect_stops_delivery_after_context_shutdown() {
        let bus = EventBus::default();
        let mut context = crate::Context::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let sink = hits.clone();
        bus.on_effect::<u64>(&context, move |_| {
            sink.fetch_add(1, Ordering::SeqCst);
        });
        bus.emit(1_u64);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        context.shutdown();
        bus.emit(2_u64);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn event_bus_isolates_panicking_listeners() {
        let bus = EventBus::default();
        let hits = Arc::new(AtomicUsize::new(0));
        bus.on::<String>(|_| panic!("listener exploded"));
        let h2 = hits.clone();
        bus.on::<String>(move |_| {
            h2.fetch_add(1, Ordering::SeqCst);
        });
        bus.emit("x".to_owned());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "second listener must still run"
        );
    }

    #[test]
    fn event_bus_does_not_cross_types() {
        let bus = EventBus::default();
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        bus.on::<String>(move |_| {
            h2.fetch_add(1, Ordering::SeqCst);
        });
        bus.emit(42_i64);
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
}
