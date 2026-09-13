//! U03 typed registration, ordering, collision, inventory and disposal contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::{ContributionKind, Plugin, compose};
use heycode_ui::{
    SERVICE_UI, UiContributionDescriptor, UiContributionId, UiRegistry, UiSlot, ui_registry_plugin,
};

struct SurfacesPlugin;

impl Plugin for SurfacesPlugin {
    fn name(&self) -> &'static str {
        "test-ui-surfaces"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            "test-ui-surfaces",
            "1.0.0",
            &[heycode_core::PluginContributionKind::UserInterface],
        )
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_UI]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
        let ui = context
            .get::<UiRegistry>(SERVICE_UI)
            .ok_or_else(|| heycode_core::CoreError::other("ui missing"))?;
        for (slot, id, title, priority, value) in [
            (UiSlot::Panel, "workspace", "Workspace", 10, 11_u32),
            (UiSlot::Panel, "secondary", "Secondary", 5, 22_u32),
            (UiSlot::Dialog, "workspace", "Workspace Dialog", 1, 33_u32),
            (UiSlot::Status, "runtime", "Runtime", 20, 44_u32),
            (UiSlot::SidePanel, "jobs", "Jobs", 8, 55_u32),
        ] {
            ui.register(
                context,
                UiContributionDescriptor::new(slot, id, title, priority)
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                Arc::new(value),
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
        }
        Ok(())
    }
}

#[test]
fn typed_slots_are_ordered_attributed_and_disposed() {
    let plugins: Vec<Box<dyn Plugin>> = vec![ui_registry_plugin(), Box::new(SurfacesPlugin)];
    let mut context = compose(&plugins).unwrap();
    let ui = context.get::<UiRegistry>(SERVICE_UI).unwrap();
    assert_eq!(
        ui.snapshot()
            .unwrap()
            .iter()
            .map(|row| (row.slot(), row.id().as_str(), row.priority()))
            .collect::<Vec<_>>(),
        [
            (UiSlot::Panel, "workspace", 10),
            (UiSlot::Panel, "secondary", 5),
            (UiSlot::Dialog, "workspace", 1),
            (UiSlot::Status, "runtime", 20),
            (UiSlot::SidePanel, "jobs", 8),
        ]
    );
    let workspace = UiContributionId::new("workspace").unwrap();
    assert_eq!(
        *ui.get::<u32>(UiSlot::Panel, &workspace).unwrap().unwrap(),
        11
    );
    assert!(
        ui.get::<String>(UiSlot::Panel, &workspace)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        *ui.get::<u32>(UiSlot::Dialog, &workspace).unwrap().unwrap(),
        33
    );

    let inventory = context.plugin_inventory().snapshot().unwrap();
    for name in [
        "panel:workspace",
        "panel:secondary",
        "dialog:workspace",
        "status:runtime",
    ] {
        assert!(inventory.contributions.iter().any(|row| {
            row.plugin == "test-ui-surfaces"
                && row.kind == ContributionKind::UiSlot
                && row.name == name
        }));
    }
    context.shutdown();
    assert!(ui.snapshot().unwrap().is_empty());
}

#[test]
fn descriptors_and_same_slot_collisions_fail_loud_without_partial_publish() {
    for (id, title) in [("Bad_Id", "Good"), ("good", ""), ("good", " bad ")] {
        assert!(UiContributionDescriptor::new(UiSlot::Panel, id, title, 0).is_err());
    }

    struct Duplicate;
    impl Plugin for Duplicate {
        fn name(&self) -> &'static str {
            "duplicate-ui"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "duplicate-ui",
                "1.0.0",
                &[heycode_core::PluginContributionKind::UserInterface],
            )
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_UI]
        }
        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let ui = context.get::<UiRegistry>(SERVICE_UI).unwrap();
            let descriptor =
                UiContributionDescriptor::new(UiSlot::Panel, "same", "Same", 0).unwrap();
            ui.register(context, descriptor.clone(), Arc::new(1_u8))
                .unwrap();
            ui.register(context, descriptor, Arc::new(2_u8))
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }
    let plugins: Vec<Box<dyn Plugin>> = vec![ui_registry_plugin(), Box::new(Duplicate)];
    let error = match compose(&plugins) {
        Ok(_) => panic!("duplicate surface must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("panel:same"), "{error}");
}
