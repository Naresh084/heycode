//! K09 verified plugin activation transaction and typed activation health.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use heycode_core::{
    ActivationStage, ContributionKind, CoreError, EventBus, Plugin, PluginActivationOutcome,
    PluginContributionKind, PluginContributionSpec, PluginDescriptor, PluginInventory, ServiceKey,
    compose, compose_activation,
};

const PROBE_SERVICE: ServiceKey = ServiceKey::new("probe-service");
const HALF_SERVICE: ServiceKey = ServiceKey::new("half-service");

/// Captures the shared inventory handle and event bus so a test can observe
/// context state that outlives an aborted composition.
struct Probe {
    inventory: Arc<Mutex<Option<PluginInventory>>>,
    bus: Arc<Mutex<Option<EventBus>>>,
}

impl Plugin for Probe {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            "probe",
            "1.0.0",
            &[
                PluginContributionKind::Service,
                PluginContributionKind::Tool,
            ],
        )
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        vec![PluginContributionSpec::new(
            ContributionKind::Tool,
            "probe-tool",
        )]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> Result<(), CoreError> {
        *self.inventory.lock().unwrap() = Some(context.plugin_inventory());
        *self.bus.lock().unwrap() = Some(context.events.clone());
        context.provide(PROBE_SERVICE, self.name(), 1_u8)
    }
}

/// Registers a service, a dynamic row, an effect and an effect-owned listener,
/// then fails. Nothing it registered may remain visible.
struct HalfApplied {
    disposed: Arc<AtomicUsize>,
    delivered: Arc<AtomicUsize>,
}

impl Plugin for HalfApplied {
    fn name(&self) -> &'static str {
        "half-applied"
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            "half-applied",
            "1.0.0",
            &[
                PluginContributionKind::Service,
                PluginContributionKind::Tool,
                PluginContributionKind::Command,
            ],
        )
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        vec![PluginContributionSpec::new(
            ContributionKind::Tool,
            "half-tool",
        )]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> Result<(), CoreError> {
        context.provide(HALF_SERVICE, self.name(), 2_u8)?;
        context.contribute(ContributionKind::Command, "half-command")?;
        let disposed = self.disposed.clone();
        context.effect(move || {
            disposed.fetch_add(1, Ordering::SeqCst);
        });
        let delivered = self.delivered.clone();
        context.events.on_effect::<String>(context, move |_| {
            delivered.fetch_add(1, Ordering::SeqCst);
        });
        Err(CoreError::other("half-applied exploded after registering"))
    }
}

#[test]
fn failed_apply_publishes_no_inventory_row_service_or_descriptor() {
    let inventory_handle = Arc::new(Mutex::new(None));
    let bus_handle = Arc::new(Mutex::new(None));
    let disposed = Arc::new(AtomicUsize::new(0));
    let delivered = Arc::new(AtomicUsize::new(0));

    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(Probe {
            inventory: inventory_handle.clone(),
            bus: bus_handle.clone(),
        }),
        Box::new(HalfApplied {
            disposed: disposed.clone(),
            delivered: delivered.clone(),
        }),
    ];

    let error = match compose(&plugins) {
        Ok(_) => panic!("a failing apply must abort composition"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("half-applied"),
        "failure must name the offending plugin: {error}"
    );

    let inventory = inventory_handle.lock().unwrap().clone().unwrap();
    let snapshot = inventory.snapshot().unwrap();
    assert!(
        snapshot
            .contributions
            .iter()
            .all(|row| row.plugin != "half-applied"),
        "no row from a failed plugin may stay published: {:?}",
        snapshot.contributions
    );
    assert!(
        snapshot
            .plugins
            .iter()
            .all(|applied| applied.descriptor.id != "half-applied"),
        "a failed plugin must not be recorded as applied"
    );
    assert_eq!(
        snapshot
            .contributions
            .iter()
            .map(|row| (row.plugin, row.kind, row.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("probe", ContributionKind::Tool, "probe-tool"),
            ("probe", ContributionKind::Service, "probe-service"),
        ],
        "the surviving inventory must be exactly what existed before the failed plugin ran"
    );

    assert_eq!(
        disposed.load(Ordering::SeqCst),
        1,
        "the failed plugin's own effect must have been disposed"
    );
    let bus = bus_handle.lock().unwrap().clone().unwrap();
    bus.emit("after rollback".to_owned());
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        0,
        "an effect-owned listener from a failed apply must not survive"
    );
}

/// Succeeds; used to fill the activated and never-reached positions.
struct Inert(&'static str);

impl Plugin for Inert {
    fn name(&self) -> &'static str {
        self.0
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(self.0, "1.0.0", &[PluginContributionKind::Service])
    }

    fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), CoreError> {
        Ok(())
    }
}

#[test]
fn activation_report_separates_activated_failed_and_never_reached() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(Inert("first")),
        Box::new(HalfApplied {
            disposed: Arc::new(AtomicUsize::new(0)),
            delivered: Arc::new(AtomicUsize::new(0)),
        }),
        Box::new(Inert("never-reached")),
    ];

    let activation = compose_activation(&plugins);
    assert!(activation.context.is_err(), "composition must fail");
    assert!(!activation.report.healthy());

    let rows = activation.report.activations();
    assert_eq!(
        rows.iter().map(|row| row.plugin).collect::<Vec<_>>(),
        vec!["first", "half-applied", "never-reached"],
        "every requested plugin gets a health row in requested order"
    );
    assert_eq!(rows[0].outcome, PluginActivationOutcome::Activated);
    assert_eq!(rows[2].outcome, PluginActivationOutcome::NotAttempted);

    let failure = match &rows[1].outcome {
        PluginActivationOutcome::Failed(failure) => failure,
        other => panic!("expected a failed activation, got {other:?}"),
    };
    assert_eq!(failure.stage, ActivationStage::Apply);
    assert!(
        failure.message.contains("half-applied exploded"),
        "{}",
        failure.message
    );

    let named = activation.report.failure().expect("one failed row");
    assert_eq!(named.plugin, "half-applied");
    assert_eq!(
        activation.report.outcome("never-reached"),
        Some(&PluginActivationOutcome::NotAttempted),
        "a plugin that never ran is not the same as one that ran and failed"
    );
}

#[test]
fn successful_composition_reports_every_plugin_activated() {
    let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(Inert("first")), Box::new(Inert("second"))];
    let activation = compose_activation(&plugins);
    assert!(activation.context.is_ok());
    assert!(activation.report.healthy());
    assert!(activation.report.failure().is_none());
    assert!(
        activation
            .report
            .activations()
            .iter()
            .all(|row| row.outcome == PluginActivationOutcome::Activated)
    );
    assert_eq!(
        activation.report.activations()[0].scope,
        heycode_core::PluginScope::BuiltIn
    );
}

#[test]
fn every_activation_stage_is_attributed_to_its_own_plugin() {
    struct MissingInject;
    impl Plugin for MissingInject {
        fn name(&self) -> &'static str {
            "missing-inject"
        }
        fn inject(&self) -> &'static [ServiceKey] {
            &[HALF_SERVICE]
        }
        fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), CoreError> {
            Ok(())
        }
    }

    struct Drifted;
    impl Plugin for Drifted {
        fn name(&self) -> &'static str {
            "drifted"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in("other-id", "1.0.0", &[PluginContributionKind::Service])
        }
        fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), CoreError> {
            Ok(())
        }
    }

    struct DuplicateRow;
    impl Plugin for DuplicateRow {
        fn name(&self) -> &'static str {
            "duplicate-row"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in("duplicate-row", "1.0.0", &[PluginContributionKind::Tool])
        }
        fn inventory(&self) -> Vec<PluginContributionSpec> {
            vec![
                PluginContributionSpec::new(ContributionKind::Tool, "same"),
                PluginContributionSpec::new(ContributionKind::Tool, "same"),
            ]
        }
        fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), CoreError> {
            Ok(())
        }
    }

    struct WrongFamily;
    impl Plugin for WrongFamily {
        fn name(&self) -> &'static str {
            "wrong-family"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in("wrong-family", "1.0.0", &[PluginContributionKind::Service])
        }
        fn apply(&self, context: &mut heycode_core::Context) -> Result<(), CoreError> {
            context.contribute(ContributionKind::Command, "outside-family")
        }
    }

    let cases: Vec<(&str, ActivationStage, Box<dyn Plugin>)> = vec![
        (
            "missing-inject",
            ActivationStage::Admission,
            Box::new(MissingInject),
        ),
        ("drifted", ActivationStage::Admission, Box::new(Drifted)),
        (
            "duplicate-row",
            ActivationStage::Declaration,
            Box::new(DuplicateRow),
        ),
        (
            "wrong-family",
            ActivationStage::Commit,
            Box::new(WrongFamily),
        ),
    ];

    for (name, stage, plugin) in cases {
        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(Inert("first")), plugin];
        let activation = compose_activation(&plugins);
        let failed = activation
            .report
            .failure()
            .unwrap_or_else(|| panic!("{name} must produce a failed activation"));
        assert_eq!(failed.plugin, name, "the offender must be named");
        match &failed.outcome {
            PluginActivationOutcome::Failed(failure) => {
                assert_eq!(failure.stage, stage, "wrong stage for {name}");
                assert!(!failure.message.is_empty(), "{name} needs safe detail");
            }
            other => panic!("{name}: expected Failed, got {other:?}"),
        }
        assert_eq!(
            activation.report.outcome("first"),
            Some(&PluginActivationOutcome::Activated),
            "{name}: an earlier healthy plugin stays activated"
        );
    }
}

#[test]
fn duplicate_plugin_name_fails_admission_for_the_second_claimant() {
    let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(Inert("twice")), Box::new(Inert("twice"))];
    let activation = compose_activation(&plugins);
    let rows = activation.report.activations();
    assert_eq!(rows[0].outcome, PluginActivationOutcome::Activated);
    match &rows[1].outcome {
        PluginActivationOutcome::Failed(failure) => {
            assert_eq!(failure.stage, ActivationStage::Admission);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Registers a listener the context cannot own, then fails: core must refuse
/// to call that rollback clean.
struct LeakyListener;

impl Plugin for LeakyListener {
    fn name(&self) -> &'static str {
        "leaky-listener"
    }

    fn apply(&self, context: &mut heycode_core::Context) -> Result<(), CoreError> {
        context.events.on::<String>(|_| {});
        Err(CoreError::other("leaky-listener exploded"))
    }
}

#[test]
fn residue_a_failed_activation_cannot_unwind_is_a_broken_activation() {
    let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(LeakyListener)];
    let activation = compose_activation(&plugins);
    let error = match activation.context {
        Ok(_) => panic!("composition must fail"),
        Err(error) => error,
    };
    match &error {
        CoreError::BrokenActivation {
            plugin,
            residue,
            cause,
        } => {
            assert_eq!(plugin, "leaky-listener");
            assert!(residue.contains("listener"), "{residue}");
            assert!(cause.contains("leaky-listener exploded"), "{cause}");
        }
        other => panic!("expected BrokenActivation, got {other:?}"),
    }
    assert!(
        error.to_string().contains("leaky-listener exploded"),
        "the original failure must survive in the message: {error}"
    );

    let failed = activation.report.failure().expect("a failed row");
    assert_eq!(failed.plugin, "leaky-listener");
    match &failed.outcome {
        PluginActivationOutcome::Failed(failure) => {
            assert_eq!(failure.stage, ActivationStage::Rollback);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn an_effect_owned_registration_rolls_back_and_keeps_the_original_failure() {
    let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(HalfApplied {
        disposed: Arc::new(AtomicUsize::new(0)),
        delivered: Arc::new(AtomicUsize::new(0)),
    })];
    let activation = compose_activation(&plugins);
    match activation.context {
        Ok(_) => panic!("composition must fail"),
        Err(CoreError::BrokenActivation { residue, .. }) => {
            panic!("an effect-owned activation must roll back cleanly, saw residue: {residue}")
        }
        Err(error) => assert!(
            error.to_string().contains("half-applied exploded"),
            "{error}"
        ),
    }
}
