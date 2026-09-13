//! K05 deterministic scope precedence and effective selection.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{PluginDirective, PluginFactories, PluginScopeLayer, resolve_scoped_plugins};
use heycode_core::{Plugin, PluginScope};

#[test]
fn shuffled_layers_resolve_by_scope_precedence_with_deterministic_order() {
    let layers = vec![
        PluginScopeLayer::new(
            PluginScope::Managed,
            vec![
                PluginDirective::disable("custom"),
                PluginDirective::enable("policy"),
            ],
        ),
        PluginScopeLayer::new(PluginScope::Session, vec![PluginDirective::enable("tools")]),
        PluginScopeLayer::new(PluginScope::Project, vec![PluginDirective::enable("mcp")]),
        PluginScopeLayer::new(
            PluginScope::User,
            vec![
                PluginDirective::disable("mcp"),
                PluginDirective::enable("custom"),
            ],
        ),
        PluginScopeLayer::new(
            PluginScope::LocalProject,
            vec![PluginDirective::disable("tools")],
        ),
    ];
    let resolved = resolve_scoped_plugins(&["settings", "tools", "mcp"], &layers).unwrap();
    assert_eq!(
        resolved
            .iter()
            .map(|row| (row.id.as_str(), row.scope))
            .collect::<Vec<_>>(),
        [
            ("settings", PluginScope::BuiltIn),
            ("mcp", PluginScope::Project),
            ("tools", PluginScope::Session),
            ("policy", PluginScope::Managed),
        ]
    );

    let mut reversed = layers;
    reversed.reverse();
    assert_eq!(
        resolve_scoped_plugins(&["settings", "tools", "mcp"], &reversed).unwrap(),
        resolved
    );
}

#[test]
fn duplicate_scope_or_plugin_directive_fails_loud() {
    let duplicate_scope = [
        PluginScopeLayer::new(PluginScope::User, vec![PluginDirective::enable("one")]),
        PluginScopeLayer::new(PluginScope::User, vec![PluginDirective::enable("two")]),
    ];
    assert!(
        resolve_scoped_plugins(&["settings"], &duplicate_scope)
            .unwrap_err()
            .to_string()
            .contains("user")
    );

    let duplicate_row = [PluginScopeLayer::new(
        PluginScope::Project,
        vec![
            PluginDirective::enable("same"),
            PluginDirective::disable("same"),
        ],
    )];
    assert!(
        resolve_scoped_plugins(&["settings"], &duplicate_row)
            .unwrap_err()
            .to_string()
            .contains("same")
    );
    assert!(resolve_scoped_plugins(&["dup", "dup"], &[]).is_err());
}

#[test]
fn resolved_scopes_reach_runtime_inventory_through_factory_build() {
    struct Named(&'static str);
    impl Plugin for Named {
        fn name(&self) -> &'static str {
            self.0
        }

        fn apply(
            &self,
            _context: &mut heycode_core::Context,
        ) -> Result<(), heycode_core::CoreError> {
            Ok(())
        }
    }

    let selected = resolve_scoped_plugins(
        &["settings"],
        &[PluginScopeLayer::new(
            PluginScope::User,
            vec![PluginDirective::enable("custom")],
        )],
    )
    .unwrap();
    let mut factories = PluginFactories::new();
    factories.register("settings", || Box::new(Named("settings")));
    factories.register("custom", || Box::new(Named("custom")));
    let scoped = factories.build_scoped(&selected).unwrap();
    let context = heycode_core::compose_scoped(&scoped).unwrap();
    assert_eq!(context.plugins(), ["settings", "custom"]);
    assert_eq!(
        context.plugin_scopes(),
        [PluginScope::BuiltIn, PluginScope::User]
    );
    assert_eq!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .plugins
            .iter()
            .map(|plugin| plugin.scope)
            .collect::<Vec<_>>(),
        [PluginScope::BuiltIn, PluginScope::User]
    );
}

#[test]
fn factory_build_stably_orders_a_late_provider_before_its_consumer() {
    const SERVICE: heycode_core::ServiceKey = heycode_core::ServiceKey::new("late-service");

    struct Provider;
    impl Plugin for Provider {
        fn name(&self) -> &'static str {
            "provider"
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE]
        }

        fn apply(
            &self,
            context: &mut heycode_core::Context,
        ) -> Result<(), heycode_core::CoreError> {
            context.provide(SERVICE, self.name(), 1_u8)
        }
    }

    struct Consumer;
    impl Plugin for Consumer {
        fn name(&self) -> &'static str {
            "consumer"
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE]
        }

        fn apply(
            &self,
            context: &mut heycode_core::Context,
        ) -> Result<(), heycode_core::CoreError> {
            context
                .get::<u8>(SERVICE)
                .map(|_| ())
                .ok_or_else(|| heycode_core::CoreError::other("late service missing"))
        }
    }

    let selected = [
        heycode_config::EffectivePluginSelection {
            id: "consumer".to_owned(),
            scope: PluginScope::User,
        },
        heycode_config::EffectivePluginSelection {
            id: "provider".to_owned(),
            scope: PluginScope::Managed,
        },
    ];
    let mut factories = PluginFactories::new();
    factories.register("consumer", || Box::new(Consumer));
    factories.register("provider", || Box::new(Provider));

    let scoped = factories.build_scoped(&selected).unwrap();
    assert_eq!(
        scoped
            .iter()
            .map(|row| (row.plugin().name(), row.scope()))
            .collect::<Vec<_>>(),
        [
            ("provider", PluginScope::Managed),
            ("consumer", PluginScope::User),
        ]
    );
    assert!(heycode_core::compose_scoped(&scoped).is_ok());
}
