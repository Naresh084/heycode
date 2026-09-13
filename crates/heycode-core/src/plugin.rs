//! The `Plugin` trait and composition entry point.

use crate::CoreResult;
use crate::context::Context;

/// A unit of capability. Plugins contribute services, listeners, and effects
/// into a [`Context`]; nothing in heycode is privileged — the binary only
/// composes plugins.
pub trait Plugin: Send + Sync {
    /// Unique plugin name (kebab-case), used in diagnostics and config.
    fn name(&self) -> &'static str;

    /// Stable plugin metadata.
    ///
    /// The compatibility default keeps name-only test/external plugins
    /// working during the descriptor migration. Every shipped built-in must
    /// override it and declare version, source and contribution families.
    fn descriptor(&self) -> crate::PluginDescriptor {
        crate::PluginDescriptor::unclassified(self.name())
    }

    /// Service keys this plugin requires to exist before its `apply` runs.
    fn inject(&self) -> &'static [crate::ServiceKey] {
        &[]
    }

    /// Service keys this plugin will publish when apply succeeds. Used by
    /// side-effect-free composition inspection; actual publication through
    /// [`crate::Context::provide`] remains authoritative.
    fn provides(&self) -> &'static [crate::ServiceKey] {
        &[]
    }

    /// Exact named rows this plugin intends to register. Services are
    /// attributed automatically by [`crate::Context::provide`]; dynamic rows
    /// discovered during apply use [`crate::Context::contribute`].
    fn inventory(&self) -> Vec<crate::PluginContributionSpec> {
        Vec::new()
    }

    /// Contribute services/listeners/effects into the context.
    ///
    /// # Errors
    /// Any load-time failure (duplicate keys, bad config) aborts composition.
    fn apply(&self, ctx: &mut Context) -> CoreResult<()>;
}

/// One concrete plugin instance selected at an explicit activation scope.
pub struct ScopedPlugin {
    scope: crate::PluginScope,
    plugin: Box<dyn Plugin>,
}

impl ScopedPlugin {
    /// Bind one plugin instance to its effective scope.
    #[must_use]
    pub fn new(scope: crate::PluginScope, plugin: Box<dyn Plugin>) -> Self {
        Self { scope, plugin }
    }

    /// Effective activation scope.
    #[must_use]
    pub const fn scope(&self) -> crate::PluginScope {
        self.scope
    }

    /// Borrow the concrete plugin.
    #[must_use]
    pub fn plugin(&self) -> &dyn Plugin {
        self.plugin.as_ref()
    }
}

/// Compose `plugins` into a live [`Context`], enforcing uniqueness of names
/// and satisfaction of every declared inject at apply time.
///
/// Each plugin activates as a verified transaction: a plugin that fails
/// publishes nothing, and composition then aborts, unwinding every prior
/// plugin's effects LIFO before returning the original error.
///
/// # Errors
/// - [`crate::CoreError::DuplicatePlugin`] on repeated names
/// - [`crate::CoreError::UnsatisfiedInject`] when requirements are missing
/// - anything a plugin's `apply` returns
/// - [`crate::CoreError::BrokenActivation`] when a failed plugin registered
///   something no rollback could remove
pub fn compose(plugins: &[Box<dyn Plugin>]) -> CoreResult<Context> {
    compose_activation(plugins).context
}

/// Compose already-resolved scoped plugin instances.
///
/// # Errors
/// Same validation and transactional failures as [`compose`].
pub fn compose_scoped(plugins: &[ScopedPlugin]) -> CoreResult<Context> {
    compose_scoped_activation(plugins).context
}

/// Compose and keep the typed activation health of every requested plugin.
///
/// Use this instead of [`compose`] when a Consumer must distinguish a plugin
/// that ran and failed from one that never ran.
pub fn compose_activation(plugins: &[Box<dyn Plugin>]) -> crate::ComposedActivation {
    compose_all(
        &plugins
            .iter()
            .map(|plugin| (crate::PluginScope::BuiltIn, plugin.as_ref()))
            .collect::<Vec<_>>(),
    )
}

/// Compose scoped instances and keep the typed activation health of each.
pub fn compose_scoped_activation(plugins: &[ScopedPlugin]) -> crate::ComposedActivation {
    compose_all(
        &plugins
            .iter()
            .map(|plugin| (plugin.scope(), plugin.plugin()))
            .collect::<Vec<_>>(),
    )
}

fn compose_all(plugins: &[(crate::PluginScope, &dyn Plugin)]) -> crate::ComposedActivation {
    let mut ctx = Context::new();
    let mut seen: Vec<&'static str> = Vec::new();
    let mut rows: Vec<crate::PluginActivation> = Vec::with_capacity(plugins.len());
    for (index, (scope, plugin)) in plugins.iter().enumerate() {
        match activate(&mut ctx, *scope, *plugin, &mut seen) {
            Ok(()) => rows.push(crate::PluginActivation {
                plugin: plugin.name(),
                scope: *scope,
                outcome: crate::PluginActivationOutcome::Activated,
            }),
            Err((stage, error)) => {
                rows.push(crate::PluginActivation {
                    plugin: plugin.name(),
                    scope: *scope,
                    outcome: crate::PluginActivationOutcome::Failed(crate::ActivationFailure {
                        stage,
                        message: error.to_string(),
                    }),
                });
                rows.extend(plugins[index + 1..].iter().map(|(scope, plugin)| {
                    crate::PluginActivation {
                        plugin: plugin.name(),
                        scope: *scope,
                        outcome: crate::PluginActivationOutcome::NotAttempted,
                    }
                }));
                // A load failure is a transaction abort: unwind every prior
                // plugin's effects before returning the original error.
                ctx.shutdown();
                return crate::ComposedActivation {
                    report: crate::ActivationReport::from_rows(rows),
                    context: Err(error),
                };
            }
        }
    }
    crate::ComposedActivation {
        report: crate::ActivationReport::from_rows(rows),
        context: Ok(ctx),
    }
}

/// Activate one plugin as a transaction: admission, then declaration, apply
/// and commit inside a rollback that is verified rather than assumed.
fn activate(
    ctx: &mut Context,
    scope: crate::PluginScope,
    plugin: &dyn Plugin,
    seen: &mut Vec<&'static str>,
) -> Result<(), (crate::ActivationStage, crate::CoreError)> {
    let descriptor = plugin.descriptor();
    if descriptor.id != plugin.name() {
        return Err((
            crate::ActivationStage::Admission,
            crate::CoreError::DescriptorIdMismatch {
                plugin: plugin.name().to_owned(),
                descriptor: descriptor.id.to_owned(),
            },
        ));
    }
    if seen.contains(&plugin.name()) {
        return Err((
            crate::ActivationStage::Admission,
            crate::CoreError::DuplicatePlugin(plugin.name().to_owned()),
        ));
    }
    if let Err(error) = ctx.verify_injects(plugin.name(), plugin.inject()) {
        return Err((crate::ActivationStage::Admission, error));
    }
    if let Err(error) = ctx.begin_activation(plugin.name()) {
        return Err((crate::ActivationStage::Admission, error));
    }

    let outcome = declare_apply_and_record(ctx, scope, plugin);
    match outcome {
        Ok(()) => {
            ctx.commit_activation();
            seen.push(plugin.name());
            Ok(())
        }
        Err((stage, error)) => match ctx.rollback_activation() {
            None => Err((stage, error)),
            Some(residue) => Err((
                crate::ActivationStage::Rollback,
                crate::CoreError::BrokenActivation {
                    plugin: plugin.name().to_owned(),
                    residue,
                    cause: error.to_string(),
                },
            )),
        },
    }
}

fn declare_apply_and_record(
    ctx: &mut Context,
    scope: crate::PluginScope,
    plugin: &dyn Plugin,
) -> Result<(), (crate::ActivationStage, crate::CoreError)> {
    for contribution in plugin.inventory() {
        ctx.contribute(contribution.kind, contribution.name)
            .map_err(|error| (crate::ActivationStage::Declaration, error))?;
    }
    plugin
        .apply(ctx)
        .map_err(|error| (crate::ActivationStage::Apply, error))?;
    ctx.record_plugin(plugin.descriptor(), scope)
        .map_err(|error| (crate::ActivationStage::Commit, error))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{CoreError, EventBus};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const COUNTER: crate::ServiceKey = crate::ServiceKey::new("counter");

    struct ProvidesCounter;
    impl Plugin for ProvidesCounter {
        fn name(&self) -> &'static str {
            "counter"
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            ctx.provide(COUNTER, self.name(), 41_i32)?;
            Ok(())
        }
    }

    struct NeedsCounter;
    impl Plugin for NeedsCounter {
        fn name(&self) -> &'static str {
            "consumer"
        }
        fn inject(&self) -> &'static [crate::ServiceKey] {
            &[COUNTER]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            assert_eq!(*ctx.get::<i32>(COUNTER).unwrap(), 41);
            Ok(())
        }
    }

    #[test]
    fn compose_applies_in_order_and_satisfies_injects() {
        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(ProvidesCounter), Box::new(NeedsCounter)];
        let ctx = compose(&plugins).unwrap();
        assert!(ctx.has(COUNTER));
        assert_eq!(ctx.owner_of(COUNTER), Some("counter"));
        assert_eq!(ctx.plugins(), ["counter", "consumer"]);
        assert_eq!(ctx.plugin_descriptors().len(), 2);
        assert_eq!(ctx.plugin_descriptors()[0].id, "counter");
        assert_eq!(
            ctx.plugin_descriptors()[0].source,
            crate::PluginSource::Unclassified
        );
    }

    #[test]
    fn descriptor_id_must_match_the_legacy_name_during_migration() {
        struct Liar;
        impl Plugin for Liar {
            fn name(&self) -> &'static str {
                "actual-name"
            }
            fn descriptor(&self) -> crate::PluginDescriptor {
                crate::PluginDescriptor::built_in(
                    "different-name",
                    "1.0.0",
                    &[crate::PluginContributionKind::Service],
                )
            }
            fn apply(&self, _ctx: &mut Context) -> CoreResult<()> {
                Ok(())
            }
        }

        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(Liar)];
        let error = match compose(&plugins) {
            Ok(_) => panic!("mismatched descriptor must fail composition"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("actual-name"), "{error}");
        assert!(error.contains("different-name"), "{error}");
    }

    #[test]
    fn compose_fails_when_inject_missing() {
        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(NeedsCounter)];
        match compose(&plugins) {
            Err(CoreError::UnsatisfiedInject { .. }) => {}
            _ => panic!("expected UnsatisfiedInject"),
        }
    }

    #[test]
    fn compose_rejects_duplicate_names() {
        let plugins: Vec<Box<dyn Plugin>> =
            vec![Box::new(ProvidesCounter), Box::new(ProvidesCounter)];
        match compose(&plugins) {
            Err(CoreError::DuplicatePlugin(n)) => assert_eq!(n, "counter"),
            _ => panic!("expected DuplicatePlugin"),
        }
    }

    #[test]
    fn composed_context_shares_one_event_bus() {
        struct BusUser;
        impl Plugin for BusUser {
            fn name(&self) -> &'static str {
                "bus-user"
            }
            fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
                ctx.events.on::<String>(|_| {});
                Ok(())
            }
        }
        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(BusUser)];
        let ctx = compose(&plugins).unwrap();
        let bus: &EventBus = &ctx.events;
        bus.emit("fine".to_owned());
    }

    #[test]
    fn plugin_effects_survive_until_shutdown() {
        struct Effectful(Arc<AtomicUsize>);
        impl Plugin for Effectful {
            fn name(&self) -> &'static str {
                "effectful"
            }
            fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
                let hits = self.0.clone();
                ctx.effect(move || {
                    hits.fetch_add(1, Ordering::SeqCst);
                });
                Ok(())
            }
        }
        let hits = Arc::new(AtomicUsize::new(0));
        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(Effectful(hits.clone()))];
        let mut ctx = compose(&plugins).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        ctx.shutdown();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// Registers one of everything an activation can publish, then fails.
    struct Greedy {
        unwound: Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl Plugin for Greedy {
        fn name(&self) -> &'static str {
            "greedy"
        }
        fn descriptor(&self) -> crate::PluginDescriptor {
            crate::PluginDescriptor::built_in(
                "greedy",
                "1.0.0",
                &[
                    crate::PluginContributionKind::Service,
                    crate::PluginContributionKind::Tool,
                    crate::PluginContributionKind::Command,
                ],
            )
        }
        fn inventory(&self) -> Vec<crate::PluginContributionSpec> {
            vec![crate::PluginContributionSpec::new(
                crate::ContributionKind::Tool,
                "greedy-tool",
            )]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            ctx.provide(GREEDY, self.name(), 7_u16)?;
            ctx.contribute(crate::ContributionKind::Command, "greedy-command")?;
            for label in ["greedy-first", "greedy-second"] {
                let unwound = self.unwound.clone();
                ctx.effect(move || unwound.lock().unwrap().push(label));
            }
            ctx.events.on_effect::<u8>(ctx, |_| {});
            Err(CoreError::other("greedy failed after registering"))
        }
    }

    const GREEDY: crate::ServiceKey = crate::ServiceKey::new("greedy-service");

    fn established() -> impl Plugin {
        struct Established;
        impl Plugin for Established {
            fn name(&self) -> &'static str {
                "established"
            }
            fn descriptor(&self) -> crate::PluginDescriptor {
                crate::PluginDescriptor::built_in(
                    "established",
                    "1.0.0",
                    &[crate::PluginContributionKind::Service],
                )
            }
            fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
                ctx.provide(COUNTER, self.name(), 41_i32)?;
                ctx.effect(|| {});
                Ok(())
            }
        }
        Established
    }

    #[test]
    fn failed_activation_leaves_the_context_observably_identical() {
        use crate::activation::ContextFingerprint;

        let mut ctx = Context::new();
        let mut seen = Vec::new();
        let first = established();
        activate(&mut ctx, crate::PluginScope::BuiltIn, &first, &mut seen).unwrap();
        let before = ContextFingerprint::capture(&ctx).unwrap();

        let unwound = Arc::new(std::sync::Mutex::new(Vec::new()));
        let greedy = Greedy {
            unwound: unwound.clone(),
        };
        let (stage, error) = activate(&mut ctx, crate::PluginScope::BuiltIn, &greedy, &mut seen)
            .expect_err("greedy must fail");
        assert_eq!(stage, crate::ActivationStage::Apply);
        assert!(error.to_string().contains("greedy"), "{error}");

        let after = ContextFingerprint::capture(&ctx).unwrap();
        assert_eq!(
            after, before,
            "a failed activation must leave services, inventory rows, applied \
             plugins, descriptors, scopes, pending effects and listeners exactly \
             as they were before it ran"
        );
        assert!(!ctx.has(GREEDY), "its service must be gone");
        assert!(ctx.has(COUNTER), "the earlier plugin's service must remain");
        assert_eq!(ctx.plugins(), ["established"]);
        assert_eq!(
            ctx.plugin_inventory()
                .snapshot()
                .unwrap()
                .contributions
                .iter()
                .map(|row| row.name.clone())
                .collect::<Vec<_>>(),
            vec!["counter".to_owned()]
        );
        assert_eq!(seen, vec!["established"], "a failed plugin claims no name");
    }

    #[test]
    fn the_activation_fingerprint_reports_every_dimension_a_plugin_can_change() {
        use crate::activation::ContextFingerprint;

        let mut ctx = Context::new();
        let empty = ContextFingerprint::capture(&ctx).unwrap();
        assert_eq!(
            empty.residue(&empty),
            None,
            "an unchanged context has no residue"
        );

        let mut seen = Vec::new();
        let first = established();
        activate(&mut ctx, crate::PluginScope::BuiltIn, &first, &mut seen).unwrap();
        let applied = ContextFingerprint::capture(&ctx).unwrap();
        let residue = empty
            .residue(&applied)
            .expect("an activated plugin is a difference");
        for dimension in [
            "services [counter]",
            "contributions [service:counter]",
            "recorded plugins",
            "plugin descriptors",
            "pending effects 0 -> 1",
        ] {
            assert!(
                residue.contains(dimension),
                "the fingerprint must observe {dimension}: {residue}"
            );
        }

        ctx.events.on::<u8>(|_| {});
        let listening = ContextFingerprint::capture(&ctx).unwrap();
        assert!(
            applied
                .residue(&listening)
                .is_some_and(|residue| residue.contains("event listeners 0 -> 1")),
            "a listener no effect owns must be observable residue"
        );
    }

    #[test]
    fn rollback_disposes_only_the_failing_activation_lifo() {
        let unwound = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut ctx = Context::new();
        let mut seen = Vec::new();
        let first = established();
        activate(&mut ctx, crate::PluginScope::BuiltIn, &first, &mut seen).unwrap();

        let earlier = unwound.clone();
        ctx.effect(move || earlier.lock().unwrap().push("earlier"));

        let greedy = Greedy {
            unwound: unwound.clone(),
        };
        activate(&mut ctx, crate::PluginScope::BuiltIn, &greedy, &mut seen).unwrap_err();
        assert_eq!(
            *unwound.lock().unwrap(),
            vec!["greedy-second", "greedy-first"],
            "rollback disposes the failing plugin's effects LIFO and nothing earlier"
        );

        ctx.shutdown();
        assert_eq!(
            *unwound.lock().unwrap(),
            vec!["greedy-second", "greedy-first", "earlier"],
            "earlier effects still unwind at shutdown, exactly once"
        );
    }

    #[test]
    fn failed_composition_rolls_back_partial_and_prior_effects_lifo() {
        struct EffectThen {
            name: &'static str,
            order: Arc<std::sync::Mutex<Vec<&'static str>>>,
            fail: bool,
        }
        impl Plugin for EffectThen {
            fn name(&self) -> &'static str {
                self.name
            }
            fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
                let name = self.name;
                let order = self.order.clone();
                ctx.effect(move || order.lock().unwrap().push(name));
                if self.fail {
                    return Err(CoreError::other(format!("{name} failed after effect")));
                }
                Ok(())
            }
        }

        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(EffectThen {
                name: "first",
                order: order.clone(),
                fail: false,
            }),
            Box::new(EffectThen {
                name: "second",
                order: order.clone(),
                fail: false,
            }),
            Box::new(EffectThen {
                name: "failing-third",
                order: order.clone(),
                fail: true,
            }),
        ];

        let error = match compose(&plugins) {
            Ok(_) => panic!("composition must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("failing-third"), "{error}");
        assert_eq!(
            *order.lock().unwrap(),
            vec!["failing-third", "second", "first"],
            "the current plugin's partial effect and all prior effects must unwind LIFO"
        );
    }
}
