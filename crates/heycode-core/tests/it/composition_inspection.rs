//! K08 side-effect-free composition diagnostics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use heycode_core::{Plugin, PluginScope, ScopedPlugin, ServiceKey, inspect_composition};

const ALPHA: ServiceKey = ServiceKey::new("alpha");
const MISSING: ServiceKey = ServiceKey::new("missing");

struct Provides {
    ran: Arc<AtomicBool>,
}

impl Plugin for Provides {
    fn name(&self) -> &'static str {
        "provides"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[ALPHA]
    }

    fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
        self.ran.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct MissingDependency;

impl Plugin for MissingDependency {
    fn name(&self) -> &'static str {
        "missing-dependency"
    }

    fn inject(&self) -> &'static [ServiceKey] {
        &[MISSING]
    }

    fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
        Ok(())
    }
}

struct Collision;

impl Plugin for Collision {
    fn name(&self) -> &'static str {
        "collision"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[ALPHA]
    }

    fn apply(&self, _context: &mut heycode_core::Context) -> Result<(), heycode_core::CoreError> {
        Ok(())
    }
}

#[test]
fn inspector_names_missing_dependency_and_service_collision_without_apply() {
    let ran = Arc::new(AtomicBool::new(false));
    let plugins = vec![
        ScopedPlugin::new(
            PluginScope::BuiltIn,
            Box::new(Provides { ran: ran.clone() }),
        ),
        ScopedPlugin::new(PluginScope::User, Box::new(MissingDependency)),
        ScopedPlugin::new(PluginScope::Managed, Box::new(Collision)),
    ];
    let report = inspect_composition(&plugins);
    assert!(!report.healthy);
    assert!(!ran.load(Ordering::SeqCst), "dry inspection invoked apply");
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "missing_dependency"
            && diagnostic.plugin.as_deref() == Some("missing-dependency")
            && diagnostic.related == ["missing"]
    }));
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "service_collision"
            && diagnostic.plugin.as_deref() == Some("collision")
            && diagnostic.related == ["alpha", "provides"]
    }));
    let human = report.render_human();
    assert!(human.contains("missing-dependency"), "{human}");
    assert!(human.contains("missing"), "{human}");
    assert!(human.contains("collision"), "{human}");
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["healthy"], false);
    assert_eq!(json["plugins"][0]["provides"][0], "alpha");
}
