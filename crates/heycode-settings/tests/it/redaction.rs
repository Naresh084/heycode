//! S15 secret-role declaration, wire-exposure verification and redaction.
//!
//! Every escape path a settings value can take out of this crate gets its own
//! canary string. A distinct canary per path means a passing assertion cannot
//! be an accident of some other path already removing the value.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_settings::{
    REDACTED_PLACEHOLDER, SettingsDefinition, SettingsDocuments, SettingsError, SettingsFieldPath,
    SettingsNamespace, SettingsSchema, SettingsService, WireExposureFault,
};
use serde_json::{Value, json};

/// Recognized credential material. Used where the detector is under test.
const CANARY_SHADOWED_LAYER: &str = "sk-canary-shadowed-layer-000000000001";
const CANARY_PUBLIC_OVERRIDE: &str = "sk-canary-public-override-0000000002";
const CANARY_SCHEMA_LITERAL: &str = "sk-canary-schema-literal-00000000003";
const CANARY_MAP_KEY: &str = "sk-canary-map-key-000000000000000004";
const CANARY_SNAPSHOT_DEBUG: &str = "sk-canary-snapshot-debug-00000000005";
const CANARY_CHANGE_DEBUG: &str = "sk-canary-change-debug-000000000006";
const CANARY_DOCUMENTS_DEBUG: &str = "sk-canary-documents-debug-0000000007";
const CANARY_SCHEMA_DEBUG: &str = "sk-canary-schema-debug-000000000008";
const CANARY_DEFINITION_DEBUG: &str = "sk-canary-definition-debug-000000009";
const CANARY_DARK_VALIDATOR: &str = "sk-canary-dark-validator-00000000010";
const CANARY_EXPOSED_VALIDATOR: &str = "sk-canary-exposed-validator-000000019";
const CANARY_WRITER_MESSAGE: &str = "sk-canary-writer-message-00000000011";

/// Deliberately NOT recognizable as credential material. Only an explicit
/// owner secret-path declaration can protect these, so a test that passes
/// with one of them proves the declared-path mechanism itself.
const QUIET_CANARY_DEFAULTS: &str = "quiet-canary-defaults-0012";
const QUIET_CANARY_BASE: &str = "quiet-canary-base-0013";
const QUIET_CANARY_USER: &str = "quiet-canary-user-0014";
const QUIET_CANARY_PROJECT: &str = "quiet-canary-project-0015";
const QUIET_CANARY_MANAGED: &str = "quiet-canary-managed-0016";
const QUIET_CANARY_NESTED: &str = "quiet-canary-nested-0017";

fn path(value: &str) -> SettingsFieldPath {
    SettingsFieldPath::new(value).unwrap()
}

fn permissive(_value: &Value) -> Result<(), String> {
    Ok(())
}

fn namespace(value: &str) -> SettingsNamespace {
    SettingsNamespace::new(value).unwrap()
}

/// Schema declaring one secret-shaped property and one plainly safe one.
fn keyed_schema() -> SettingsSchema {
    SettingsSchema::new(
        json!({
            "type": "object",
            "properties": {
                "api_key": {"type": "string"},
                "endpoint": {"type": "string"}
            }
        }),
        json!({"endpoint": "https://example.invalid"}),
        permissive,
    )
    .unwrap()
}

#[test]
fn unprovably_safe_secret_schema_fails_wire_exposure() {
    let service = SettingsService::new(SettingsDocuments::new());
    let mut context = heycode_core::Context::new();
    let exposed = namespace("unprovable-owner");

    let error = service
        .register(
            &context,
            SettingsDefinition::new(exposed.clone(), keyed_schema().with_wire_exposure()),
        )
        .unwrap_err();

    match &error {
        SettingsError::UnprovableWireExposure {
            namespace: reported,
            path: reported_path,
            fault,
        } => {
            assert_eq!(reported, "unprovable-owner");
            assert_eq!(reported_path, "api_key");
            assert_eq!(*fault, WireExposureFault::SecretShapedKey);
        }
        other => panic!("expected UnprovableWireExposure, got {other:?}"),
    }
    assert!(
        service.get(&exposed).unwrap().is_none(),
        "a failed exposure proof must publish no registration"
    );
    assert!(service.describe().unwrap().is_empty());

    let dark = namespace("dark-owner");
    let snapshot = service
        .register(&context, SettingsDefinition::new(dark, keyed_schema()))
        .unwrap();
    assert!(
        !snapshot.wire_exposed(),
        "the identical schema is fine while it stays dark"
    );
    assert!(snapshot.wire_projection().is_none());
    context.shutdown();
}

#[test]
fn declared_secret_path_discharges_the_proof_and_redacts_every_projected_layer() {
    let owner = namespace("declared-owner");
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            owner.clone(),
            json!({"api_key": QUIET_CANARY_USER, "endpoint": "user"}),
        )
        .unwrap();
    documents
        .set_project(owner.clone(), json!({"api_key": QUIET_CANARY_PROJECT}))
        .unwrap();
    documents
        .set_managed(owner.clone(), json!({"api_key": QUIET_CANARY_MANAGED}))
        .unwrap();
    let service = SettingsService::new(documents);
    let mut context = heycode_core::Context::new();

    let schema = SettingsSchema::new(
        json!({
            "type": "object",
            "properties": {"api_key": {"type": "string"}, "endpoint": {"type": "string"}}
        }),
        json!({"api_key": QUIET_CANARY_DEFAULTS, "endpoint": "default"}),
        permissive,
    )
    .unwrap()
    .with_secret_path(path("api_key"))
    .with_wire_exposure();
    let definition = SettingsDefinition::new(owner.clone(), schema)
        .with_base(json!({"api_key": QUIET_CANARY_BASE}))
        .unwrap();

    let snapshot = service.register(&context, definition).unwrap();
    let projection = snapshot.wire_projection().expect("proof discharged");

    for (label, value) in [
        ("defaults", Some(projection.defaults())),
        ("base", projection.base()),
        ("user", projection.user()),
        ("project", projection.project()),
        ("managed", projection.managed()),
        ("resolved", Some(projection.resolved())),
    ] {
        let value = value.unwrap_or_else(|| panic!("{label} layer must be projected"));
        assert_eq!(
            value["api_key"], REDACTED_PLACEHOLDER,
            "{label} layer must project the placeholder"
        );
    }
    assert_eq!(projection.user().unwrap()["endpoint"], "user");
    assert_eq!(
        projection.redacted_paths().to_vec(),
        vec!["api_key".to_owned()]
    );

    let rendered = serde_json::to_string(&json!({
        "schema": projection.schema(),
        "defaults": projection.defaults(),
        "base": projection.base(),
        "user": projection.user(),
        "project": projection.project(),
        "managed": projection.managed(),
        "resolved": projection.resolved(),
    }))
    .unwrap();
    for canary in [
        QUIET_CANARY_DEFAULTS,
        QUIET_CANARY_BASE,
        QUIET_CANARY_USER,
        QUIET_CANARY_PROJECT,
        QUIET_CANARY_MANAGED,
    ] {
        assert!(
            !rendered.contains(canary),
            "projected layers leaked {canary}"
        );
    }
    assert_eq!(
        snapshot.user().unwrap()["api_key"],
        QUIET_CANARY_USER,
        "the owner still reads its own values in process"
    );
    context.shutdown();
}

#[test]
fn credential_material_in_a_shadowed_layer_still_fails_wire_exposure() {
    let owner = namespace("shadowed-owner");
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(owner.clone(), json!({"note": CANARY_SHADOWED_LAYER}))
        .unwrap();
    documents
        .set_project(owner.clone(), json!({"note": "harmless"}))
        .unwrap();
    let service = SettingsService::new(documents);
    let mut context = heycode_core::Context::new();
    let schema = SettingsSchema::new(
        json!({"type": "object", "properties": {"note": {"type": "string"}}}),
        json!({"note": "default"}),
        permissive,
    )
    .unwrap()
    .with_wire_exposure();

    let error = service
        .register(&context, SettingsDefinition::new(owner.clone(), schema))
        .unwrap_err();

    assert!(
        matches!(
            &error,
            SettingsError::UnprovableWireExposure { path, fault, .. }
                if path == "note" && *fault == WireExposureFault::CredentialMaterial
        ),
        "a higher layer hiding the value from `resolved` must not hide it from the verifier: {error:?}"
    );
    assert!(
        !format!("{error}").contains(CANARY_SHADOWED_LAYER),
        "the fault must be a closed reason, never the offending value"
    );
    assert!(!format!("{error:?}").contains(CANARY_SHADOWED_LAYER));
    context.shutdown();
}

#[test]
fn credential_material_cannot_be_attested_public() {
    let owner = namespace("override-owner");
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(owner.clone(), json!({"note": CANARY_PUBLIC_OVERRIDE}))
        .unwrap();
    let service = SettingsService::new(documents);
    let mut context = heycode_core::Context::new();
    let schema = SettingsSchema::new(
        json!({"type": "object", "properties": {"note": {"type": "string"}}}),
        json!({}),
        permissive,
    )
    .unwrap()
    .with_public_path(path("note"))
    .with_wire_exposure();

    let error = service
        .register(&context, SettingsDefinition::new(owner, schema))
        .unwrap_err();

    assert!(
        matches!(
            &error,
            SettingsError::UnprovableWireExposure { fault, .. }
                if *fault == WireExposureFault::CredentialMaterial
        ),
        "an owner attestation may discharge a name, never recognized material: {error:?}"
    );
    context.shutdown();
}

#[test]
fn public_path_attestation_discharges_a_secret_shaped_key() {
    let owner = namespace("budget-owner");
    let service = SettingsService::new(SettingsDocuments::new());
    let mut context = heycode_core::Context::new();
    let schema = SettingsSchema::new(
        json!({"type": "object", "properties": {"token_limit": {"type": "integer"}}}),
        json!({"token_limit": 4096}),
        permissive,
    )
    .unwrap();

    let undischarged = service
        .register(
            &context,
            SettingsDefinition::new(owner.clone(), schema.clone().with_wire_exposure()),
        )
        .unwrap_err();
    assert!(matches!(
        undischarged,
        SettingsError::UnprovableWireExposure {
            fault: WireExposureFault::SecretShapedKey,
            ..
        }
    ));

    let snapshot = service
        .register(
            &context,
            SettingsDefinition::new(
                owner,
                schema
                    .with_public_path(path("token_limit"))
                    .with_wire_exposure(),
            ),
        )
        .unwrap();
    let projection = snapshot.wire_projection().expect("attested per path");
    assert_eq!(projection.resolved()["token_limit"], 4096);
    assert!(projection.redacted_paths().is_empty());
    context.shutdown();
}

#[test]
fn one_path_declared_both_secret_and_public_fails_loud() {
    let owner = namespace("contradiction-owner");
    let service = SettingsService::new(SettingsDocuments::new());
    let mut context = heycode_core::Context::new();
    let schema = SettingsSchema::new(json!({"type": "object"}), json!({}), permissive)
        .unwrap()
        .with_secret_path(path("auth.value"))
        .with_public_path(path("auth.value"));

    let error = service
        .register(&context, SettingsDefinition::new(owner, schema))
        .unwrap_err();

    assert!(
        matches!(
            &error,
            SettingsError::UnprovableWireExposure { path, fault, .. }
                if path == "auth.value" && *fault == WireExposureFault::ContradictoryRole
        ),
        "an ambiguous declaration is a defect even before exposure: {error:?}"
    );
    context.shutdown();
}

#[test]
fn secret_role_wins_under_a_public_ancestor_and_matches_wildcards() {
    let owner = namespace("servers-owner");
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            owner.clone(),
            json!({"servers": {"alpha": {"password": QUIET_CANARY_NESTED, "url": "alpha.invalid"}}}),
        )
        .unwrap();
    let service = SettingsService::new(documents);
    let mut context = heycode_core::Context::new();
    let schema = SettingsSchema::new(
        json!({
            "type": "object",
            "properties": {
                "servers": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "password": {"type": "string"},
                            "url": {"type": "string"}
                        }
                    }
                }
            }
        }),
        json!({"servers": {}}),
        permissive,
    )
    .unwrap()
    .with_public_path(path("servers"))
    .with_secret_path(path("servers.*.password"))
    .with_wire_exposure();

    let snapshot = service
        .register(&context, SettingsDefinition::new(owner, schema))
        .unwrap();
    let projection = snapshot.wire_projection().expect("proof discharged");

    assert_eq!(
        projection.resolved()["servers"]["alpha"]["password"],
        REDACTED_PLACEHOLDER
    );
    assert_eq!(
        projection.resolved()["servers"]["alpha"]["url"],
        "alpha.invalid"
    );
    assert_eq!(
        projection.redacted_paths().to_vec(),
        vec!["servers.alpha.password".to_owned()]
    );
    context.shutdown();
}

#[test]
fn schema_metadata_literals_are_verified_like_layer_values() {
    let owner = namespace("literal-owner");
    let service = SettingsService::new(SettingsDocuments::new());
    let mut context = heycode_core::Context::new();
    let schema = SettingsSchema::new(
        json!({
            "type": "object",
            "properties": {"endpoint": {"type": "string", "default": CANARY_SCHEMA_LITERAL}}
        }),
        json!({}),
        permissive,
    )
    .unwrap()
    .with_wire_exposure();

    let error = service
        .register(&context, SettingsDefinition::new(owner, schema))
        .unwrap_err();

    assert!(
        matches!(
            &error,
            SettingsError::UnprovableWireExposure { fault, .. }
                if *fault == WireExposureFault::CredentialMaterial
        ),
        "schema metadata is projected verbatim, so it is verified too: {error:?}"
    );
    assert!(!format!("{error}").contains(CANARY_SCHEMA_LITERAL));
    context.shutdown();
}

#[test]
fn credential_material_used_as_a_map_key_is_caught_and_never_rendered() {
    let owner = namespace("keyed-owner");
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(owner.clone(), json!({"entries": {CANARY_MAP_KEY: "value"}}))
        .unwrap();
    let service = SettingsService::new(documents);
    let mut context = heycode_core::Context::new();
    let schema = SettingsSchema::new(
        json!({"type": "object", "properties": {"entries": {"type": "object"}}}),
        json!({"entries": {}}),
        permissive,
    )
    .unwrap()
    .with_wire_exposure();

    let error = service
        .register(&context, SettingsDefinition::new(owner, schema))
        .unwrap_err();

    assert!(
        matches!(
            &error,
            SettingsError::UnprovableWireExposure { fault, .. }
                if *fault == WireExposureFault::CredentialMaterial
        ),
        "a secret can be a key, not only a value: {error:?}"
    );
    assert!(
        matches!(&error, SettingsError::UnprovableWireExposure { path, .. } if path == "entries.[REDACTED]"),
        "the reported path must redact a material-shaped key segment: {error:?}"
    );
    assert!(!format!("{error}").contains(CANARY_MAP_KEY));
    assert!(!format!("{error:?}").contains(CANARY_MAP_KEY));
    context.shutdown();
}

#[test]
fn debug_rendering_redacts_secret_material_in_every_value_carrying_type() {
    let owner = namespace("debug-owner");
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            owner.clone(),
            json!({"note": CANARY_DOCUMENTS_DEBUG, "quiet": QUIET_CANARY_USER}),
        )
        .unwrap();
    assert!(
        !format!("{documents:?}").contains(CANARY_DOCUMENTS_DEBUG),
        "provider documents render before any schema exists"
    );

    let schema = SettingsSchema::new(
        json!({"type": "object", "properties": {"note": {"type": "string"}}}),
        json!({"note": CANARY_SCHEMA_DEBUG, "quiet": QUIET_CANARY_DEFAULTS}),
        permissive,
    )
    .unwrap()
    .with_secret_path(path("quiet"));
    assert!(!format!("{schema:?}").contains(CANARY_SCHEMA_DEBUG));
    assert!(
        !format!("{schema:?}").contains(QUIET_CANARY_DEFAULTS),
        "a declared secret path redacts in debug even without exposure"
    );

    let definition = SettingsDefinition::new(owner.clone(), schema)
        .with_base(json!({"note": CANARY_DEFINITION_DEBUG}))
        .unwrap();
    assert!(!format!("{definition:?}").contains(CANARY_DEFINITION_DEBUG));

    let mut snapshot_documents = SettingsDocuments::new();
    snapshot_documents
        .set_user(
            owner.clone(),
            json!({"note": CANARY_SNAPSHOT_DEBUG, "quiet": QUIET_CANARY_USER}),
        )
        .unwrap();
    let service = SettingsService::with_writer(snapshot_documents, Arc::new(AcceptingWriter));
    let mut context = heycode_core::Context::new();
    let snapshot = service.register(&context, definition).unwrap();
    let rendered = format!("{snapshot:?}");
    assert!(!rendered.contains(CANARY_SNAPSHOT_DEBUG));
    assert!(!rendered.contains(QUIET_CANARY_USER));

    let (sender, receiver) = std::sync::mpsc::channel();
    service
        .watch(&context, &owner, move |change| {
            let _ = sender.send(format!("{change:?}"));
        })
        .unwrap();
    service
        .replace_user(
            &owner,
            json!({"note": CANARY_CHANGE_DEBUG, "quiet": QUIET_CANARY_USER}),
            Some(snapshot.revision()),
        )
        .unwrap();
    let observed = receiver.recv().unwrap();
    assert!(!observed.contains(CANARY_CHANGE_DEBUG));
    assert!(!observed.contains(QUIET_CANARY_USER));
    context.shutdown();
}

struct AcceptingWriter;

impl heycode_settings::SettingsWriter for AcceptingWriter {
    fn persist_user(&self, _namespace: &SettingsNamespace, _section: &Value) -> Result<(), String> {
        Ok(())
    }
}

struct EchoingWriter;

impl heycode_settings::SettingsWriter for EchoingWriter {
    fn persist_user(&self, _namespace: &SettingsNamespace, section: &Value) -> Result<(), String> {
        Err(format!("refused to persist {section}"))
    }
}

#[test]
fn owner_supplied_messages_never_carry_layer_values_into_errors() {
    fn echoing(value: &Value) -> Result<(), String> {
        if value.get("note").is_some() {
            return Err(format!("rejected {value}"));
        }
        Ok(())
    }

    let exposed = namespace("echo-exposed");
    let dark = namespace("echo-dark");
    let writable = namespace("echo-writer");
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(exposed.clone(), json!({"note": CANARY_EXPOSED_VALIDATOR}))
        .unwrap();
    documents
        .set_user(dark.clone(), json!({"note": CANARY_DARK_VALIDATOR}))
        .unwrap();
    let service = SettingsService::with_writer(documents, Arc::new(EchoingWriter));
    let mut context = heycode_core::Context::new();

    let exposed_error = service
        .register(
            &context,
            SettingsDefinition::new(
                exposed,
                SettingsSchema::new(json!({"type": "object"}), json!({}), echoing)
                    .unwrap()
                    .with_wire_exposure(),
            ),
        )
        .unwrap_err();
    assert!(
        matches!(
            &exposed_error,
            SettingsError::UnprovableWireExposure { fault, .. }
                if *fault == WireExposureFault::CredentialMaterial
        ),
        "an exposed namespace is verified before an owner validator can echo a value: {exposed_error:?}"
    );
    assert!(!format!("{exposed_error}").contains(CANARY_EXPOSED_VALIDATOR));

    let dark_error = service
        .register(
            &context,
            SettingsDefinition::new(
                dark,
                SettingsSchema::new(json!({"type": "object"}), json!({}), echoing).unwrap(),
            ),
        )
        .unwrap_err();
    assert!(matches!(dark_error, SettingsError::InvalidResolved { .. }));
    assert!(
        !format!("{dark_error}").contains(CANARY_DARK_VALIDATOR),
        "recognized material is scrubbed out of owner validator messages"
    );

    service
        .register(
            &context,
            SettingsDefinition::new(
                writable.clone(),
                SettingsSchema::new(json!({"type": "object"}), json!({}), permissive).unwrap(),
            ),
        )
        .unwrap();
    let writer_error = service
        .replace_user(&writable, json!({"note": CANARY_WRITER_MESSAGE}), Some(0))
        .unwrap_err();
    assert!(matches!(writer_error, SettingsError::Provider { .. }));
    assert!(
        !format!("{writer_error}").contains(CANARY_WRITER_MESSAGE),
        "recognized material is scrubbed out of provider messages"
    );
    context.shutdown();
}

#[test]
fn field_paths_are_validated_newtypes() {
    assert!(SettingsFieldPath::new("").is_err());
    assert!(SettingsFieldPath::new("a..b").is_err());
    assert!(SettingsFieldPath::new("a.*x").is_err());
    assert_eq!(path("servers.*.password").to_string(), "servers.*.password");
    assert!(matches!(
        SettingsFieldPath::new("a..b").unwrap_err(),
        SettingsError::InvalidFieldPath { .. }
    ));
}

/// Introduced after registration, through the two paths that mutate a layer.
const CANARY_LATE_WRITE: &str = "sk-canary-late-write-000000000000021";
const CANARY_LATE_RELOAD: &str = "sk-canary-late-reload-00000000000022";

#[derive(Default)]
struct CountingWriter(std::sync::atomic::AtomicUsize);

impl heycode_settings::SettingsWriter for CountingWriter {
    fn persist_user(&self, _namespace: &SettingsNamespace, _section: &Value) -> Result<(), String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn material_introduced_after_registration_is_refused_before_it_can_be_persisted() {
    let owner = namespace("late-owner");
    let writer = Arc::new(CountingWriter::default());
    let service = SettingsService::with_writer(SettingsDocuments::new(), writer.clone());
    let mut context = heycode_core::Context::new();
    let mut watch_context = heycode_core::Context::new();
    let schema = SettingsSchema::new(
        json!({"type": "object", "properties": {"note": {"type": "string"}}}),
        json!({"note": "default"}),
        permissive,
    )
    .unwrap()
    .with_wire_exposure();
    let snapshot = service
        .register(&context, SettingsDefinition::new(owner.clone(), schema))
        .unwrap();
    assert!(snapshot.wire_exposed());
    let (sender, receiver) = std::sync::mpsc::channel();
    service
        .watch(&watch_context, &owner, move |change| {
            let _ = sender.send(change);
        })
        .unwrap();

    let write_error = service
        .replace_user(&owner, json!({"note": CANARY_LATE_WRITE}), Some(0))
        .unwrap_err();
    assert!(
        matches!(
            &write_error,
            SettingsError::UnprovableWireExposure { fault, .. }
                if *fault == WireExposureFault::CredentialMaterial
        ),
        "an exposed namespace re-proves itself on every commit: {write_error:?}"
    );
    assert_eq!(
        writer.0.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a refused write never reaches the durable writer"
    );

    let mut reloaded = SettingsDocuments::new();
    reloaded
        .set_user(owner.clone(), json!({"note": CANARY_LATE_RELOAD}))
        .unwrap();
    assert!(service.publish_documents(reloaded).is_err());

    let current = service.get(&owner).unwrap().unwrap();
    assert_eq!(current.revision(), 0);
    assert_eq!(current.resolved()["note"], "default");
    assert!(
        receiver.try_recv().is_err(),
        "neither refusal notifies a watcher"
    );
    watch_context.shutdown();
    context.shutdown();
}
