//! Namespace schema, defaults, base, and apply timing.

use std::sync::Arc;

use serde_json::Value;

use crate::redaction::{FieldRoles, redact_for_debug};
use crate::{SettingsApplies, SettingsError, SettingsFieldPath, SettingsLayer, SettingsNamespace};

type Validator = Arc<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;

/// JSON-schema metadata, immutable defaults, and authoritative validation.
#[derive(Clone)]
pub struct SettingsSchema {
    document: Value,
    defaults: Value,
    validator: Validator,
    wire_exposed: bool,
    roles: FieldRoles,
}

impl std::fmt::Debug for SettingsSchema {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsSchema")
            .field("document", &redact_for_debug(&self.document, &self.roles))
            .field("defaults", &redact_for_debug(&self.defaults, &self.roles))
            .field("wire_exposed", &self.wire_exposed)
            .field("roles", &self.roles)
            .finish_non_exhaustive()
    }
}

impl SettingsSchema {
    /// Define schema metadata, defaults, and an owner validator.
    ///
    /// The JSON schema is metadata for future settings surfaces. The validator
    /// is authoritative in-process and runs for defaults and every resolution.
    ///
    /// # Errors
    /// Metadata/defaults must be objects and defaults must pass `validator`.
    pub fn new(
        document: Value,
        defaults: Value,
        validator: impl Fn(&Value) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, SettingsError> {
        if !document.is_object() {
            return Err(SettingsError::SchemaMustBeObject);
        }
        if !defaults.is_object() {
            return Err(SettingsError::InvalidDefaults {
                message: "defaults must be a JSON object".to_owned(),
            });
        }
        validator(&defaults).map_err(|message| SettingsError::InvalidDefaults { message })?;
        Ok(Self {
            document,
            defaults,
            validator: Arc::new(validator),
            wire_exposed: false,
            roles: FieldRoles::default(),
        })
    }

    /// Attest that every otherwise undeclared value accepted by this schema
    /// is non-secret and may cross local UI/app-server inspection boundaries.
    ///
    /// This is deliberately opt-in, and it is a claim the service verifies
    /// rather than accepts: registration fails when any projected path is not
    /// provably safe. Credential values belong in the credentials service.
    #[must_use]
    pub fn with_wire_exposure(mut self) -> Self {
        self.wire_exposed = true;
        self
    }

    /// Declare that one path holds secret material.
    ///
    /// The path and everything beneath it is replaced by
    /// [`crate::REDACTED_PLACEHOLDER`] in every projection and rendered
    /// diagnostic, exposed or not. A secret declaration outranks a public one
    /// on the same subtree, so an owner can expose a map and redact one field
    /// inside it.
    #[must_use]
    pub fn with_secret_path(mut self, path: SettingsFieldPath) -> Self {
        self.roles.declare_secret(path);
        self
    }

    /// Attest that one path is non-secret despite its name.
    ///
    /// This discharges the key-name screen for that path and everything
    /// beneath it. It cannot discharge recognized credential material: a value
    /// that matches a published credential format is never projected.
    #[must_use]
    pub fn with_public_path(mut self, path: SettingsFieldPath) -> Self {
        self.roles.declare_public(path);
        self
    }

    pub(crate) fn validate(&self, value: &Value) -> Result<(), String> {
        (self.validator)(value)
    }

    pub(crate) fn document(&self) -> &Value {
        &self.document
    }

    pub(crate) fn defaults(&self) -> &Value {
        &self.defaults
    }

    pub(crate) const fn wire_exposed(&self) -> bool {
        self.wire_exposed
    }

    pub(crate) const fn roles(&self) -> &FieldRoles {
        &self.roles
    }
}

/// One plugin-owned namespace definition.
#[derive(Clone)]
pub struct SettingsDefinition {
    pub(crate) namespace: SettingsNamespace,
    pub(crate) schema: SettingsSchema,
    pub(crate) base: Option<Value>,
    pub(crate) override_layer: Option<Value>,
    pub(crate) applies: SettingsApplies,
}

impl std::fmt::Debug for SettingsDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsDefinition")
            .field("namespace", &self.namespace)
            .field("schema", &self.schema)
            .field(
                "base",
                &self
                    .base
                    .as_ref()
                    .map(|base| redact_for_debug(base, self.schema.roles())),
            )
            .field(
                "override",
                &self
                    .override_layer
                    .as_ref()
                    .map(|layer| redact_for_debug(layer, self.schema.roles())),
            )
            .field("applies", &self.applies)
            .finish()
    }
}

impl SettingsDefinition {
    /// Define a namespace from its schema/default contract.
    #[must_use]
    pub fn new(namespace: SettingsNamespace, schema: SettingsSchema) -> Self {
        Self {
            namespace,
            schema,
            base: None,
            override_layer: None,
            applies: SettingsApplies::Live,
        }
    }

    /// Add the composition/deployment base layer.
    ///
    /// # Errors
    /// The base must be a JSON object so recursive layering is deterministic.
    pub fn with_base(mut self, base: Value) -> Result<Self, SettingsError> {
        if !base.is_object() {
            return Err(SettingsError::LayerMustBeObject {
                namespace: self.namespace.to_string(),
                layer: SettingsLayer::Base,
            });
        }
        self.base = Some(base);
        Ok(self)
    }

    /// Add this process's command-line override layer.
    ///
    /// The layer resolves above user and project values and below managed
    /// locks. It is ephemeral: the first in-session user write to the
    /// namespace drops it, so `/model` still works after `--model`.
    ///
    /// # Errors
    /// The override must be a JSON object so recursive layering is
    /// deterministic.
    pub fn with_override(mut self, layer: Value) -> Result<Self, SettingsError> {
        if !layer.is_object() {
            return Err(SettingsError::LayerMustBeObject {
                namespace: self.namespace.to_string(),
                layer: SettingsLayer::Override,
            });
        }
        self.override_layer = Some(layer);
        Ok(self)
    }

    /// Declare when this owner applies changes.
    #[must_use]
    pub fn with_applies(mut self, applies: SettingsApplies) -> Self {
        self.applies = applies;
        self
    }
}
