//! K04 exact contribution attribution and collision contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    ContributionKind, CoreError, Plugin, PluginContributionSpec, PluginScope, ScopedPlugin,
    ServiceKey, compose, compose_scoped,
};

const ALPHA: ServiceKey = ServiceKey::new("alpha-service");

struct Alpha;

impl Plugin for Alpha {
    fn name(&self) -> &'static str {
        "alpha"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            "alpha",
            "1.0.0",
            &[
                heycode_core::PluginContributionKind::Service,
                heycode_core::PluginContributionKind::Tool,
            ],
        )
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        vec![PluginContributionSpec::new(ContributionKind::Tool, "read")]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> Result<(), CoreError> {
        context.provide(ALPHA, self.name(), 1_u8)
    }
}

#[test]
fn composition_attributes_automatic_services_and_declared_exact_rows() {
    let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(Alpha)];
    let context = compose(&plugins).unwrap();
    let snapshot = context.plugin_inventory().snapshot().unwrap();
    assert_eq!(snapshot.plugins.len(), 1);
    assert_eq!(snapshot.plugins[0].descriptor.id, "alpha");
    assert_eq!(snapshot.contributions.len(), 2);
    assert_eq!(snapshot.contributions[0].plugin, "alpha");
    assert_eq!(snapshot.contributions[0].kind, ContributionKind::Tool);
    assert_eq!(snapshot.contributions[0].name, "read");
    assert_eq!(snapshot.contributions[1].plugin, "alpha");
    assert_eq!(snapshot.contributions[1].kind, ContributionKind::Service);
    assert_eq!(snapshot.contributions[1].name, "alpha-service");
}

#[test]
fn duplicate_exact_kind_and_name_fails_naming_both_plugins() {
    struct Duplicate;
    impl Plugin for Duplicate {
        fn name(&self) -> &'static str {
            "duplicate"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "duplicate",
                "1.0.0",
                &[heycode_core::PluginContributionKind::Tool],
            )
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            vec![PluginContributionSpec::new(ContributionKind::Tool, "read")]
        }

        fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), CoreError> {
            Ok(())
        }
    }

    let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(Alpha), Box::new(Duplicate)];
    let error = match compose(&plugins) {
        Ok(_) => panic!("duplicate contribution must fail composition"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("alpha"), "{error}");
    assert!(error.contains("duplicate"), "{error}");
    assert!(error.contains("read"), "{error}");
}

#[test]
fn exact_rows_must_belong_to_a_declared_broad_descriptor_family() {
    struct Mismatch;
    impl Plugin for Mismatch {
        fn name(&self) -> &'static str {
            "mismatch"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "mismatch",
                "1.0.0",
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            vec![PluginContributionSpec::new(
                ContributionKind::Command,
                "wrong-family",
            )]
        }

        fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), CoreError> {
            Ok(())
        }
    }

    let error = match compose(&[Box::new(Mismatch) as Box<dyn Plugin>]) {
        Ok(_) => panic!("descriptor family mismatch must fail composition"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("mismatch"), "{error}");
    assert!(error.contains("command"), "{error}");
}

#[test]
fn scoped_composition_records_activation_scope_separately_from_source() {
    let plugins = vec![ScopedPlugin::new(PluginScope::User, Box::new(Alpha))];
    let context = compose_scoped(&plugins).unwrap();
    let snapshot = context.plugin_inventory().snapshot().unwrap();
    assert_eq!(snapshot.plugins[0].descriptor.id, "alpha");
    assert_eq!(snapshot.plugins[0].descriptor.source.as_str(), "built_in");
    assert_eq!(snapshot.plugins[0].scope, PluginScope::User);
}
