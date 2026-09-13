//! Register/dispose properties: exact disposers unwound LIFO, and the line
//! between an effect-owned registration and an owner-lifetime one.
//!
//! Every sequence here composes a generated world of plugins that register into
//! four registries a plugin can reach — the context's service map, its exact
//! contribution inventory, the shared [`heycode_core::EventBus`], and a
//! [`heycode_core::Waterfall`] seam the plugins did **not** create — and then
//! compares every one of them against an independently computed model.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_core::{
    ActivationStage, Context, ContributionKind, CoreError, CoreResult, EventBus, Plugin,
    PluginActivationOutcome, PluginContributionKind, PluginDescriptor, PluginInventory,
    PluginScope, ScopedPlugin, ServiceKey, compose_scoped_activation,
};

use super::lab::{
    Chain, DropWitness, LayerProbe, ListenerProbe, Log, Ping, Rng, live_layers, runtime, shrink,
};

/// How many seeded sequences each property explores.
const SEQUENCES: u64 = 256;

/// Plugin identities available to a generated world. `Plugin::name` is
/// `&'static str`, so the pool is fixed and a world is at most this wide.
const PLUGIN_NAMES: [&str; 6] = ["lab-a", "lab-b", "lab-c", "lab-d", "lab-e", "lab-f"];

/// Service keys available to a generated world, for the same reason.
const SERVICE_KEYS: [ServiceKey; 8] = [
    ServiceKey::new("lab/service-0"),
    ServiceKey::new("lab/service-1"),
    ServiceKey::new("lab/service-2"),
    ServiceKey::new("lab/service-3"),
    ServiceKey::new("lab/service-4"),
    ServiceKey::new("lab/service-5"),
    ServiceKey::new("lab/service-6"),
    ServiceKey::new("lab/service-7"),
];

/// Both families every generated plugin may contribute into, so a world is
/// never rejected by the descriptor/exact-row family check for a reason that
/// has nothing to do with lifecycle.
const FAMILIES: &[PluginContributionKind] = &[
    PluginContributionKind::Service,
    PluginContributionKind::Tool,
];

/// One generated operation. Deliberately flat and self-describing: any
/// subsequence is still a runnable sequence, which is what lets the shrinker
/// delete freely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// Begin a new plugin; later registrations belong to it.
    Plugin,
    /// One registration by the plugin currently being built.
    Register(Kind),
    /// The plugin currently being built refuses to activate here.
    Fail,
}

/// What a plugin can register, split along the axis that matters: whether the
/// registration leaves with the plugin or lives as long as the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `EventBus::on_effect` — effect-owned.
    OwnedListener,
    /// `EventBus::on` — bus-lifetime.
    SharedListener,
    /// `Waterfall::push_effect` — effect-owned.
    OwnedLayer,
    /// `Waterfall::push_shared` — chain-lifetime.
    SharedLayer,
    /// `Context::effect` — a plain disposer.
    Disposer,
    /// `Context::provide` — a service, rolled back by the transaction.
    Service,
    /// `Context::contribute` — an exact inventory row, likewise.
    Row,
}

impl Kind {
    /// Every kind, for generation and coverage.
    const ALL: [Self; 7] = [
        Self::OwnedListener,
        Self::SharedListener,
        Self::OwnedLayer,
        Self::SharedLayer,
        Self::Disposer,
        Self::Service,
        Self::Row,
    ];

    /// True when the registration is supposed to leave with its plugin.
    const fn effect_owned(self) -> bool {
        matches!(
            self,
            Self::OwnedListener | Self::OwnedLayer | Self::Disposer
        )
    }
}

/// One normalized registration with the identity the model tracks it by.
#[derive(Clone, Copy, Debug)]
struct Step {
    id: u32,
    kind: Kind,
    key: Option<ServiceKey>,
}

/// One normalized plugin.
struct PluginPlan {
    name: &'static str,
    steps: Vec<Step>,
    fails: bool,
}

/// Turn a raw operation sequence into a runnable world.
///
/// Every rule here exists so that deleting operations cannot produce something
/// unrunnable: a registration with no plugin is dropped, a plugin past the name
/// pool is dropped, a registration after the current plugin has already failed
/// is dropped because it could never have run, and a service registration past
/// the key pool degrades to a disposer rather than colliding.
fn normalize(ops: &[Op]) -> Vec<PluginPlan> {
    let mut plan: Vec<PluginPlan> = Vec::new();
    let mut next_id: u32 = 0;
    let mut next_key: usize = 0;
    let mut already_failing = false;
    for op in ops {
        match op {
            Op::Plugin => {
                if plan.len() < PLUGIN_NAMES.len() {
                    plan.push(PluginPlan {
                        name: PLUGIN_NAMES[plan.len()],
                        steps: Vec::new(),
                        fails: false,
                    });
                }
            }
            Op::Register(kind) => {
                let Some(current) = plan.last_mut() else {
                    continue;
                };
                if current.fails {
                    continue;
                }
                let (kind, key) = match kind {
                    Kind::Service if next_key < SERVICE_KEYS.len() => {
                        let key = SERVICE_KEYS[next_key];
                        next_key += 1;
                        (Kind::Service, Some(key))
                    }
                    Kind::Service => (Kind::Disposer, None),
                    other => (*other, None),
                };
                current.steps.push(Step {
                    id: next_id,
                    kind,
                    key,
                });
                next_id += 1;
            }
            Op::Fail => {
                if already_failing {
                    continue;
                }
                let Some(current) = plan.last_mut() else {
                    continue;
                };
                current.fails = true;
                already_failing = true;
            }
        }
    }
    plan
}

/// Everything a generated world registers into, held outside the context so it
/// survives the context's death and can be inspected afterwards.
#[derive(Clone)]
struct World {
    chain: Arc<Chain>,
    dropped: Log,
    ran: Log,
    live_listeners: Log,
    bus: Arc<Mutex<Option<EventBus>>>,
    inventory: Arc<Mutex<Option<PluginInventory>>>,
}

impl World {
    fn new() -> Self {
        Self {
            chain: Arc::new(Chain::default()),
            dropped: Log::default(),
            ran: Log::default(),
            live_listeners: Log::default(),
            bus: Arc::new(Mutex::new(None)),
            inventory: Arc::new(Mutex::new(None)),
        }
    }

    /// Which listeners are still receiving, in delivery order.
    fn listeners(&self) -> Vec<u32> {
        let bus = self.bus.lock().unwrap().clone();
        self.live_listeners.clear();
        if let Some(bus) = bus {
            bus.emit(Ping);
        }
        self.live_listeners.entries()
    }

    /// Which exact rows the inventory still attributes, in registration order.
    fn rows(&self) -> Vec<String> {
        let inventory = self.inventory.lock().unwrap().clone();
        inventory.map_or_else(Vec::new, |inventory| {
            inventory
                .snapshot()
                .unwrap()
                .contributions
                .iter()
                .map(|row| format!("{}/{}:{}", row.plugin, row.kind, row.name))
                .collect()
        })
    }
}

/// A plugin that performs exactly the generated registrations, then optionally
/// refuses.
struct ProbePlugin {
    name: &'static str,
    steps: Vec<Step>,
    fails: bool,
    world: World,
}

impl Plugin for ProbePlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(self.name, "0.0.0", FAMILIES)
    }

    fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
        // Captured before anything is registered, so even a world whose first
        // plugin fails leaves the bus and the inventory observable.
        *self.world.bus.lock().unwrap() = Some(ctx.events.clone());
        *self.world.inventory.lock().unwrap() = Some(ctx.plugin_inventory());
        for step in &self.steps {
            let id = step.id;
            match step.kind {
                Kind::OwnedListener => {
                    let probe =
                        ListenerProbe::new(id, &self.world.live_listeners, &self.world.dropped);
                    ctx.events.on_effect::<Ping>(ctx, move |_| probe.hit());
                }
                Kind::SharedListener => {
                    let probe =
                        ListenerProbe::new(id, &self.world.live_listeners, &self.world.dropped);
                    ctx.events.on::<Ping>(move |_| probe.hit());
                }
                Kind::OwnedLayer => {
                    self.world
                        .chain
                        .push_effect(ctx, LayerProbe::new(id, &self.world.dropped));
                }
                Kind::SharedLayer => {
                    self.world
                        .chain
                        .push_shared(LayerProbe::new(id, &self.world.dropped));
                }
                Kind::Disposer => {
                    let ran = self.world.ran.clone();
                    let witness = DropWitness::new(id, &self.world.dropped);
                    ctx.effect(move || {
                        ran.push(id);
                        drop(witness);
                    });
                }
                Kind::Service => {
                    ctx.provide(step.key.unwrap(), self.name, id)?;
                }
                Kind::Row => {
                    ctx.contribute(ContributionKind::Tool, format!("row-{id}"))?;
                }
            }
        }
        if self.fails {
            return Err(CoreError::other(format!(
                "{} refuses to activate",
                self.name
            )));
        }
        Ok(())
    }
}

macro_rules! claim {
    ($condition:expr, $($message:tt)*) => {
        if !$condition {
            return Err(format!($($message)*));
        }
    };
}

/// What the model says the world should look like.
struct Expected {
    failing: Option<usize>,
    broken: bool,
    rows: Vec<String>,
    live_listeners: Vec<u32>,
    live_layers: Vec<u32>,
    surviving_listeners: Vec<u32>,
    surviving_layers: Vec<u32>,
    disposal: Vec<u32>,
    ran: Vec<u32>,
}

/// Derive the expected end state of every registry from the plan alone.
fn model(plan: &[PluginPlan]) -> Expected {
    let failing = plan.iter().position(|plugin| plugin.fails);
    // A failing plugin's own registrations run and are then rolled back; every
    // later plugin never runs at all.
    let executed = failing.map_or(plan.len(), |index| index + 1);
    // Only plugins that committed keep their exact rows.
    let committed = failing.unwrap_or(plan.len());

    let mut rows = Vec::new();
    for plugin in &plan[..committed] {
        for step in &plugin.steps {
            match step.kind {
                Kind::Service => rows.push(format!(
                    "{}/service:{}",
                    plugin.name,
                    step.key.unwrap().as_str()
                )),
                Kind::Row => rows.push(format!("{}/tool:row-{}", plugin.name, step.id)),
                _ => {}
            }
        }
    }

    let mut owned = Vec::new();
    let mut disposers = Vec::new();
    let mut live_listeners = Vec::new();
    let mut live_layers = Vec::new();
    let mut surviving_listeners = Vec::new();
    let mut surviving_layers = Vec::new();
    for plugin in &plan[..executed] {
        for step in &plugin.steps {
            if step.kind.effect_owned() {
                owned.push(step.id);
            }
            match step.kind {
                Kind::OwnedListener => live_listeners.push(step.id),
                Kind::SharedListener => {
                    live_listeners.push(step.id);
                    surviving_listeners.push(step.id);
                }
                Kind::OwnedLayer => live_layers.push(step.id),
                Kind::SharedLayer => {
                    live_layers.push(step.id);
                    surviving_layers.push(step.id);
                }
                Kind::Disposer => disposers.push(step.id),
                Kind::Service | Kind::Row => {}
            }
        }
    }

    Expected {
        failing,
        // Rollback re-captures the context fingerprint, which counts listeners
        // but cannot see a seam that lives behind a service. So a failing
        // plugin's bus-lifetime listener is caught as residue and its
        // chain-lifetime layer is not — the documented gap, modelled rather
        // than glossed over.
        broken: failing.is_some_and(|index| {
            plan[index]
                .steps
                .iter()
                .any(|step| step.kind == Kind::SharedListener)
        }),
        rows,
        live_listeners,
        live_layers,
        disposal: owned.iter().rev().copied().collect(),
        ran: disposers.iter().rev().copied().collect(),
        surviving_listeners,
        surviving_layers,
    }
}

/// Run one generated sequence and compare every registry against the model.
fn check(runtime: &tokio::runtime::Runtime, ops: &[Op]) -> Result<(), String> {
    let plan = normalize(ops);
    let expected = model(&plan);
    let world = World::new();
    let plugins: Vec<ScopedPlugin> = plan
        .iter()
        .map(|plugin| {
            ScopedPlugin::new(
                PluginScope::BuiltIn,
                Box::new(ProbePlugin {
                    name: plugin.name,
                    steps: plugin.steps.clone(),
                    fails: plugin.fails,
                    world: world.clone(),
                }) as Box<dyn Plugin>,
            )
        })
        .collect();

    let composed = compose_scoped_activation(&plugins);

    claim!(
        composed.report.activations().len() == plan.len(),
        "the report must carry one row per requested plugin"
    );
    for (index, row) in composed.report.activations().iter().enumerate() {
        let want = match expected.failing {
            Some(failing) if index == failing => "failed",
            Some(failing) if index > failing => "not-attempted",
            _ => "activated",
        };
        let got = match &row.outcome {
            PluginActivationOutcome::Activated => "activated",
            PluginActivationOutcome::Failed(_) => "failed",
            PluginActivationOutcome::NotAttempted => "not-attempted",
            _ => "unrecognized",
        };
        claim!(
            got == want,
            "plugin `{}` reports {got}, expected {want}",
            row.plugin
        );
        if let (Some(failing), PluginActivationOutcome::Failed(failure)) =
            (expected.failing, &row.outcome)
        {
            let stage = if expected.broken {
                ActivationStage::Rollback
            } else {
                ActivationStage::Apply
            };
            claim!(
                index == failing && failure.stage == stage,
                "plugin `{}` failed at stage {}, expected {stage}",
                row.plugin,
                failure.stage
            );
        }
    }

    match (expected.failing, composed.context) {
        (None, Err(error)) => {
            return Err(format!(
                "a world with no failing plugin was rejected: {error}"
            ));
        }
        (Some(_), Ok(_)) => {
            return Err("a world with a failing plugin composed anyway".to_owned());
        }
        (Some(failing), Err(error)) => {
            let rendered = error.to_string();
            let culprit = plan[failing].name;
            match (&error, expected.broken) {
                (CoreError::BrokenActivation { plugin, .. }, true) => claim!(
                    plugin == culprit,
                    "residue must name `{culprit}`, named `{plugin}`"
                ),
                (CoreError::BrokenActivation { .. }, false) => {
                    return Err(format!(
                        "a fully rolled back activation was reported as residue: {rendered}"
                    ));
                }
                (_, true) => {
                    return Err(format!(
                        "a bus-lifetime listener left by `{culprit}` was not reported as residue: \
                         {rendered}"
                    ));
                }
                (_, false) => claim!(
                    rendered.contains(culprit),
                    "the failure must name `{culprit}`: {rendered}"
                ),
            }
        }
        (None, Ok(mut context)) => {
            // Alive: every registration of every plugin is published.
            claim!(
                world.dropped.entries().is_empty(),
                "nothing may be disposed while the world is live, saw {:?}",
                world.dropped.entries()
            );
            claim!(
                world.ran.entries().is_empty(),
                "no disposer may run while the world is live"
            );
            let listeners = world.listeners();
            claim!(
                listeners == expected.live_listeners,
                "live listeners are {listeners:?}, expected {:?}",
                expected.live_listeners
            );
            let layers = live_layers(runtime, &world.chain);
            claim!(
                layers == expected.live_layers,
                "live seam layers are {layers:?}, expected {:?}",
                expected.live_layers
            );
            claim!(
                world.chain.shared_layer_count() == expected.live_layers.len(),
                "shared_layer_count disagrees with the chain it counts"
            );

            context.shutdown();
        }
    }

    // Whatever happened, the same end state is required: effect-owned
    // registrations gone in LIFO order, owner-lifetime ones untouched.
    let dropped = world.dropped.entries();
    claim!(
        dropped == expected.disposal,
        "disposal order is {dropped:?}, expected LIFO {:?}",
        expected.disposal
    );
    let ran = world.ran.entries();
    claim!(
        ran == expected.ran,
        "disposers ran {ran:?}, expected {:?}",
        expected.ran
    );
    let listeners = world.listeners();
    claim!(
        listeners == expected.surviving_listeners,
        "surviving listeners are {listeners:?}, expected {:?}",
        expected.surviving_listeners
    );
    let layers = live_layers(runtime, &world.chain);
    claim!(
        layers == expected.surviving_layers,
        "surviving seam layers are {layers:?}, expected {:?}",
        expected.surviving_layers
    );
    claim!(
        world.chain.shared_layer_count() == expected.surviving_layers.len(),
        "shared_layer_count disagrees with the chain it counts"
    );
    let rows = world.rows();
    claim!(
        rows == expected.rows,
        "inventory rows are {rows:?}, expected {:?}",
        expected.rows
    );
    Ok(())
}

/// One seeded sequence of plugins and registrations.
fn generate(rng: &mut Rng) -> Vec<Op> {
    let mut ops = vec![Op::Plugin];
    let length = 4 + rng.below(20);
    for _ in 0..length {
        if rng.chance(18) {
            ops.push(Op::Plugin);
        } else if rng.chance(8) {
            ops.push(Op::Fail);
        } else {
            ops.push(Op::Register(Kind::ALL[rng.below(Kind::ALL.len())]));
        }
    }
    ops
}

/// Report the shortest sequence that still disagrees with the model.
fn report(runtime: &tokio::runtime::Runtime, seed: u64, ops: &[Op], failure: &str) -> String {
    let minimal = shrink(ops, |candidate| check(runtime, candidate).is_err());
    let detail = check(runtime, &minimal)
        .err()
        .unwrap_or_else(|| failure.to_owned());
    format!(
        "seed {seed}: {detail}\n  minimal sequence ({} ops): {minimal:?}\n  original ({} ops)",
        minimal.len(),
        ops.len()
    )
}

/// K09's claim is that one activation is a transaction over everything the
/// context owns, and that a registration into a registry the plugin did not
/// create carries its own disposer. Both are claims about *arbitrary* orders,
/// so this generates the orders: for any sequence of plugins and registrations,
/// the service map, the exact inventory, the event bus and a seam chain all end
/// in exactly the state the model derives from the sequence alone — and the
/// disposal order is exactly LIFO.
#[test]
fn every_generated_register_dispose_order_leaves_each_registry_exactly_as_the_model_says() {
    let runtime = runtime();
    for seed in 1..=SEQUENCES {
        let ops = generate(&mut Rng::seeded(seed));
        if let Err(failure) = check(&runtime, &ops) {
            panic!("{}", report(&runtime, seed, &ops, &failure));
        }
    }
}

/// A generator that stopped producing one of the kinds would quietly turn this
/// lab into decoration, and the effect-owned/owner-lifetime split is the whole
/// point — a corpus of only one side cannot tell them apart. This pins that the
/// corpus actually reaches every kind and both composition outcomes.
#[test]
fn the_generated_corpus_reaches_every_registration_kind_and_both_composition_outcomes() {
    let mut per_kind = [0_usize; Kind::ALL.len()];
    let mut rejected = 0_usize;
    let mut with_residue = 0_usize;
    let mut clean_rollback = 0_usize;
    for seed in 1..=SEQUENCES {
        let plan = normalize(&generate(&mut Rng::seeded(seed)));
        let expected = model(&plan);
        if expected.failing.is_some() {
            rejected += 1;
            if expected.broken {
                with_residue += 1;
            } else {
                clean_rollback += 1;
            }
        }
        for plugin in &plan {
            for step in &plugin.steps {
                let index = Kind::ALL
                    .iter()
                    .position(|kind| *kind == step.kind)
                    .expect("every step kind is in ALL");
                per_kind[index] += 1;
            }
        }
    }
    for (index, count) in per_kind.iter().enumerate() {
        assert!(
            *count >= 20,
            "{:?} appears only {count} times in {SEQUENCES} sequences",
            Kind::ALL[index]
        );
    }
    assert!(
        rejected >= SEQUENCES as usize / 8,
        "only {rejected} of {SEQUENCES} sequences contain a failing plugin"
    );
    assert!(
        rejected <= SEQUENCES as usize * 7 / 8,
        "{rejected} of {SEQUENCES} sequences contain a failing plugin; \
         the clean path is barely explored"
    );
    // Both halves of the rollback verdict must occur, or the property cannot
    // tell a residue report from a clean unwind.
    assert!(
        with_residue >= 20,
        "only {with_residue} sequences leave residue a rollback cannot remove"
    );
    assert!(
        clean_rollback >= 20,
        "only {clean_rollback} sequences roll a failing plugin back completely"
    );
}
