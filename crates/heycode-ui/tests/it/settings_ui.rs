//! S14: what a derived settings form may and may not contain.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::Context;
use heycode_settings::{
    SettingsDefinition, SettingsDocuments, SettingsFieldPath, SettingsNamespace, SettingsSchema,
    SettingsService, SettingsSnapshot, SettingsWriter,
};
use heycode_ui::settings_ui::{
    FieldOrigin, SettingsField, SettingsSurface, SettingsUiRegistry, UnrenderableReason,
    derive_form,
};
use heycode_ui::{UiContributionId, UiRegistryError};
use serde_json::{Value, json};

struct NoWriter;
impl SettingsWriter for NoWriter {
    fn persist_user(&self, _n: &SettingsNamespace, _s: &Value) -> Result<(), String> {
        Ok(())
    }
}

fn namespace() -> SettingsNamespace {
    SettingsNamespace::new("demo").unwrap()
}

/// Build a registered namespace and hand back its snapshot.
fn snapshot(
    schema: Value,
    defaults: Value,
    secret_paths: &[&str],
    wire_exposed: bool,
    user: Option<Value>,
    project: Option<Value>,
    managed: Option<Value>,
) -> (Arc<SettingsService>, Arc<SettingsSnapshot>, Context) {
    let mut documents = SettingsDocuments::new();
    if let Some(user) = user {
        documents.set_user(namespace(), user).unwrap();
    }
    if let Some(project) = project {
        documents.set_project(namespace(), project).unwrap();
    }
    if let Some(managed) = managed {
        documents.set_managed(namespace(), managed).unwrap();
    }
    let mut built = SettingsSchema::new(schema, defaults, |_| Ok(())).unwrap();
    for path in secret_paths {
        built = built.with_secret_path(SettingsFieldPath::new(*path).unwrap());
    }
    if wire_exposed {
        built = built.with_wire_exposure();
    }
    let service = Arc::new(SettingsService::with_writer(
        documents,
        Arc::new(NoWriter) as Arc<dyn SettingsWriter>,
    ));
    let context = Context::default();
    service
        .register(&context, SettingsDefinition::new(namespace(), built))
        .unwrap();
    let snapshot = service.get(&namespace()).unwrap().unwrap();
    (service, snapshot, context)
}

fn field<'a>(fields: &'a [SettingsField], path: &str) -> &'a SettingsField {
    fields
        .iter()
        .find(|field| field.path() == path)
        .unwrap_or_else(|| panic!("no field at `{path}`"))
}

#[test]
fn a_schema_becomes_typed_controls_in_schema_order() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {
            "enabled": {"type": "boolean"},
            "label": {"type": "string"},
            "retries": {"type": "integer"},
            "mode": {"type": "string", "enum": ["off", "readonly", "workspace"]}
        }}),
        json!({"enabled": true, "label": "hello", "retries": 3, "mode": "off"}),
        &[],
        false,
        None,
        None,
        None,
    );
    let form = derive_form(&snapshot);
    assert_eq!(form.namespace(), "demo");
    assert!(matches!(
        field(form.fields(), "enabled"),
        SettingsField::Toggle { value: true, .. }
    ));
    assert!(matches!(
        field(form.fields(), "label"),
        SettingsField::Text { value, .. } if value == "hello"
    ));
    assert!(matches!(
        field(form.fields(), "retries"),
        SettingsField::Number { value, .. } if value == "3"
    ));
    let SettingsField::Choice {
        options, selected, ..
    } = field(form.fields(), "mode")
    else {
        panic!("mode must be a choice");
    };
    assert_eq!(options, &["off", "readonly", "workspace"]);
    assert_eq!(selected.as_deref(), Some("off"));
}

/// The security property, and the reason `Secret` has no value field: there is
/// nowhere for a secret to be, so no renderer can leak one.
#[test]
fn a_declared_secret_becomes_a_valueless_control_and_its_value_never_enters_the_form() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {
            "endpoint": {"type": "string"},
            "handshake": {"type": "string"}
        }}),
        json!({"endpoint": "https://h.test", "handshake": ""}),
        &["handshake"],
        true,
        Some(json!({"handshake": "super-secret-canary"})),
        None,
        None,
    );
    let form = derive_form(&snapshot);

    let SettingsField::Secret { configured, .. } = field(form.fields(), "handshake") else {
        panic!("a declared secret must render as a secret control");
    };
    assert!(*configured, "the UI may know that a value exists");

    let rendered = format!("{form:?}");
    assert!(
        !rendered.contains("super-secret-canary"),
        "no secret may reach the form at all: {rendered}"
    );
}

/// The name screen is the second line of defence, for a field nobody declared.
#[test]
fn a_credential_shaped_field_name_becomes_a_secret_control_without_any_declaration() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {
            "api_key": {"type": "string"},
            "label": {"type": "string"}
        }}),
        json!({"api_key": "", "label": "x"}),
        &[],
        false,
        Some(json!({"api_key": "sk-canary-value"})),
        None,
        None,
    );
    let form = derive_form(&snapshot);
    assert!(matches!(
        field(form.fields(), "api_key"),
        SettingsField::Secret { .. }
    ));
    assert!(!format!("{form:?}").contains("sk-canary-value"));
    assert!(matches!(
        field(form.fields(), "label"),
        SettingsField::Text { .. }
    ));
}

/// A secret with nothing configured must still say so, or the UI cannot tell a
/// user whether they need to set one.
#[test]
fn an_unset_secret_reports_that_it_is_not_configured() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {"api_key": {"type": "string"}}}),
        json!({}),
        &[],
        false,
        None,
        None,
        None,
    );
    let form = derive_form(&snapshot);
    let SettingsField::Secret { configured, .. } = field(form.fields(), "api_key") else {
        panic!("expected a secret control");
    };
    assert!(!configured);
}

/// A managed value is shown read-only rather than editable-then-rejected —
/// discovering a lock by being denied is a poor experience.
#[test]
fn a_managed_value_reports_its_origin_and_is_not_editable() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {
            "label": {"type": "string"},
            "other": {"type": "string"}
        }}),
        json!({"label": "default", "other": "default"}),
        &[],
        false,
        Some(json!({"other": "mine"})),
        None,
        Some(json!({"label": "corporate"})),
    );
    let form = derive_form(&snapshot);

    let SettingsField::Text { origin, value, .. } = field(form.fields(), "label") else {
        panic!("expected text");
    };
    assert_eq!(*origin, FieldOrigin::Managed);
    assert_eq!(value, "corporate");
    assert!(!field(form.fields(), "label").editable());

    assert!(field(form.fields(), "other").editable());
}

/// Precedence only means something where layers overlap. Each path here exists
/// in *several* layers at once, so a test that merely put one value in each
/// layer would pass under a reversed ordering — and did, until a mutation
/// caught it.
#[test]
fn origin_reports_the_highest_precedence_layer_that_carries_a_value() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {
            "all_three": {"type": "string"},
            "project_and_user": {"type": "string"},
            "user_only": {"type": "string"},
            "untouched": {"type": "string"}
        }}),
        json!({"all_three": "d", "project_and_user": "d", "user_only": "d", "untouched": "d"}),
        &[],
        false,
        Some(json!({"all_three": "user", "project_and_user": "user", "user_only": "user"})),
        Some(json!({"all_three": "project", "project_and_user": "project"})),
        Some(json!({"all_three": "managed"})),
    );
    let form = derive_form(&snapshot);
    for (path, expected, effective) in [
        ("all_three", FieldOrigin::Managed, "managed"),
        ("project_and_user", FieldOrigin::Project, "project"),
        ("user_only", FieldOrigin::User, "user"),
        ("untouched", FieldOrigin::Default, "d"),
    ] {
        let SettingsField::Text { origin, value, .. } = field(form.fields(), path) else {
            panic!("expected text at {path}");
        };
        assert_eq!(*origin, expected, "wrong origin for `{path}`");
        assert_eq!(value, effective, "wrong effective value for `{path}`");
    }
}

/// A field the user cannot see is a field they cannot fix, so an unrenderable
/// construct is reported rather than silently dropped.
#[test]
fn an_unrenderable_construct_is_reported_not_dropped() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {
            "fine": {"type": "string"},
            "freeform": {"type": "object"},
            "mystery": {"type": "geometry"}
        }}),
        json!({"fine": "x"}),
        &[],
        false,
        None,
        None,
        None,
    );
    let form = derive_form(&snapshot);
    assert_eq!(form.fields().len(), 3, "nothing may be dropped");
    assert!(matches!(
        field(form.fields(), "freeform"),
        SettingsField::Unrenderable {
            reason: UnrenderableReason::OpenEnded,
            ..
        }
    ));
    assert!(matches!(
        field(form.fields(), "mystery"),
        SettingsField::Unrenderable {
            reason: UnrenderableReason::UnsupportedType,
            ..
        }
    ));
    assert_eq!(form.unrenderable().len(), 2);
    assert!(!field(form.fields(), "freeform").editable());
}

#[test]
fn nested_objects_are_walked_and_paths_are_dotted() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {
            "outer": {"type": "object", "properties": {
                "inner": {"type": "boolean"}
            }}
        }}),
        json!({"outer": {"inner": true}}),
        &[],
        false,
        None,
        None,
        None,
    );
    let form = derive_form(&snapshot);
    assert!(matches!(
        field(form.fields(), "outer.inner"),
        SettingsField::Toggle { value: true, .. }
    ));
}

/// A choice whose stored value is not among the options must not be presented
/// as selected — that would make an invalid value look blessed.
#[test]
fn a_choice_holding_a_value_outside_its_options_reports_nothing_selected() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {
            "mode": {"type": "string", "enum": ["off", "on"]}
        }}),
        json!({"mode": "off"}),
        &[],
        false,
        Some(json!({"mode": "sideways"})),
        None,
        None,
    );
    let form = derive_form(&snapshot);
    let SettingsField::Choice { selected, .. } = field(form.fields(), "mode") else {
        panic!("expected a choice");
    };
    assert_eq!(*selected, None);
}

#[test]
fn a_namespace_defaults_to_a_derived_form_and_a_custom_panel_takes_over() {
    let (_s, snapshot, _c) = snapshot(
        json!({"type": "object", "properties": {"label": {"type": "string"}}}),
        json!({"label": "x"}),
        &[],
        false,
        None,
        None,
        None,
    );
    let registry = SettingsUiRegistry::new();
    assert!(matches!(
        registry.surface(&snapshot).unwrap(),
        SettingsSurface::Derived(_)
    ));

    let panel = UiContributionId::new("demo-settings").unwrap();
    let mut context = heycode_core::Context::new();
    registry
        .register_custom(&context, &namespace(), panel.clone())
        .unwrap();
    let SettingsSurface::Custom(id) = registry.surface(&snapshot).unwrap() else {
        panic!("a registered custom panel must take over");
    };
    assert_eq!(id, panel);
    context.shutdown();
    assert!(matches!(
        registry.surface(&snapshot).unwrap(),
        SettingsSurface::Derived(_)
    ));
}

/// Two plugins silently competing to own one namespace's UI is a collision, not
/// a merge.
#[test]
fn a_second_custom_panel_for_one_namespace_is_refused() {
    let registry = SettingsUiRegistry::new();
    let context = heycode_core::Context::new();
    registry
        .register_custom(
            &context,
            &namespace(),
            UiContributionId::new("first").unwrap(),
        )
        .unwrap();
    let error = registry
        .register_custom(
            &context,
            &namespace(),
            UiContributionId::new("second").unwrap(),
        )
        .unwrap_err();
    assert!(matches!(error, UiRegistryError::Duplicate { .. }));
}
