//! Route selection and plugin-owned settings schema.

use std::collections::{BTreeMap, BTreeSet};

use heycode_settings::{SettingsDefinition, SettingsError, SettingsNamespace, SettingsSchema};
use serde_json::{Value, json};

use crate::RoutingError;

/// Routing settings namespace.
///
/// # Errors
/// Static namespace validation failure.
pub fn settings_namespace() -> Result<SettingsNamespace, SettingsError> {
    SettingsNamespace::new("routing")
}

/// Resolve only the highest-precedence explicitly persisted runtime id before
/// the full routing schema/plugin is composed.
///
/// This is used solely to choose credential preflight/session metadata. Full
/// tuple validation remains composition-owned by [`routing_definition`].
///
/// # Errors
/// A present runtime field must satisfy the same bounded id contract.
pub fn requested_runtime(
    documents: &heycode_settings::SettingsDocuments,
) -> Result<Option<String>, RoutingError> {
    if requires_setup(documents)? {
        return Ok(Some("native".to_owned()));
    }
    if persisted_pending_connection(documents)?.is_some() {
        return Ok(Some("native".into()));
    }
    let namespace = settings_namespace().map_err(|_| RoutingError::RegistryUnavailable)?;
    let value = documents
        .project_section(&namespace)
        .and_then(|section| section.get("runtime"))
        .or_else(|| {
            documents
                .user_section(&namespace)
                .and_then(|section| section.get("runtime"))
        });
    let Some(value) = value else {
        return Ok(None);
    };
    let runtime = value
        .as_str()
        .ok_or(RoutingError::InvalidSelection("runtime"))?;
    RoutingSelection::new(runtime, "placeholder", "placeholder", None)
        .map(|selection| Some(selection.runtime().to_owned()))
}

/// Whether durable routing contains a selected connection, independent of current readiness.
///
/// # Errors
/// Invalid settings namespace or persisted runtime metadata.
pub fn has_persisted_connection(
    documents: &heycode_settings::SettingsDocuments,
) -> Result<bool, RoutingError> {
    Ok(!requires_setup(documents)?
        && (persisted_field(documents, "provider")?.is_some()
            || persisted_field(documents, "pending_connection")?.is_some()
            || requested_runtime(documents)?.is_some_and(|runtime| runtime != "native")))
}

/// Whether an explicit logout requires first-run connection setup.
///
/// This durable latch is independent of credential discovery: environment
/// variables and vendor-owned login stores cannot silently clear it.
///
/// # Errors
/// A persisted non-boolean latch is invalid routing state.
pub fn requires_setup(
    documents: &heycode_settings::SettingsDocuments,
) -> Result<bool, RoutingError> {
    let namespace = settings_namespace().map_err(|_| RoutingError::RegistryUnavailable)?;
    [
        documents.user_section(&namespace),
        documents.project_section(&namespace),
        documents.managed_section(&namespace),
    ]
    .into_iter()
    .try_fold(false, |required, layer| {
        let layer_required = layer_requires_setup(layer)?;
        Ok(required || layer_required)
    })
}

pub(crate) fn snapshot_requires_setup(
    snapshot: &heycode_settings::SettingsSnapshot,
) -> Result<bool, RoutingError> {
    [snapshot.user(), snapshot.project(), snapshot.managed()]
        .into_iter()
        .try_fold(false, |required, layer| {
            let layer_required = layer_requires_setup(layer)?;
            Ok(required || layer_required)
        })
}

fn layer_requires_setup(layer: Option<&Value>) -> Result<bool, RoutingError> {
    layer
        .and_then(Value::as_object)
        .and_then(|object| object.get("setup_required"))
        .map_or(Ok(false), |required| {
            required
                .as_bool()
                .ok_or(RoutingError::InvalidSelection("setup_required"))
        })
}

fn persisted_field(
    documents: &heycode_settings::SettingsDocuments,
    key: &str,
) -> Result<Option<Value>, RoutingError> {
    let namespace = settings_namespace().map_err(|_| RoutingError::RegistryUnavailable)?;
    Ok(documents
        .project_section(&namespace)
        .and_then(|section| section.get(key))
        .or_else(|| {
            documents
                .user_section(&namespace)
                .and_then(|section| section.get(key))
        })
        .cloned())
}

fn persisted_pending_connection(
    documents: &heycode_settings::SettingsDocuments,
) -> Result<Option<RoutingSelection>, RoutingError> {
    pending_connection(persisted_field(documents, "pending_connection")?.as_ref())
}

pub(crate) fn pending_connection(
    value: Option<&Value>,
) -> Result<Option<RoutingSelection>, RoutingError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or(RoutingError::InvalidSelection("pending_connection"))?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "provider" | "model" | "endpoint" | "credential_reference" | "parameters"
        )
    }) || !object.contains_key("provider")
        || !object.contains_key("model")
    {
        return Err(RoutingError::InvalidSelection("pending_connection"));
    }
    let reference = reference_value(object.get("credential_reference"))?;
    RoutingSelection::new(
        "native",
        object
            .get("provider")
            .and_then(Value::as_str)
            .ok_or(RoutingError::InvalidSelection("pending provider"))?,
        object
            .get("model")
            .and_then(Value::as_str)
            .ok_or(RoutingError::InvalidSelection("pending model"))?,
        None,
    )?
    .with_endpoint(endpoint_value(object.get("endpoint"))?)
    .and_then(|selection| selection.with_parameters(parameters_value(object.get("parameters"))?))
    .map(|selection| Some(selection.with_credential_reference(reference)))
}

fn parameters_value(value: Option<&Value>) -> Result<BTreeMap<String, String>, RoutingError> {
    match value {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| RoutingError::InvalidSelection("parameters")),
    }
}

fn reference_value(
    value: Option<&Value>,
) -> Result<Option<heycode_credentials::CredentialReference>, RoutingError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => heycode_credentials::CredentialReference::new(value.clone())
            .map(Some)
            .map_err(|_| RoutingError::InvalidSelection("credential_reference")),
        Some(_) => Err(RoutingError::InvalidSelection("credential_reference")),
    }
}

fn endpoint_value(value: Option<&Value>) -> Result<Option<String>, RoutingError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(RoutingError::InvalidSelection("endpoint")),
    }
}

/// Resolve startup connection intent above the caller's configured fallback.
///
/// A pending connection is applied only by a newly composed world. Ordinary
/// routing reads continue to describe the currently mounted provider.
///
/// # Errors
/// Malformed persisted fields or pending connection tuples fail before activation.
pub fn requested_connection(
    documents: &heycode_settings::SettingsDocuments,
    base: &RoutingSelection,
) -> Result<RoutingSelection, RoutingError> {
    if requires_setup(documents)? {
        return Ok(base.clone());
    }
    if let Some(pending) = persisted_pending_connection(documents)? {
        return Ok(pending);
    }
    let text = |key: &'static str, fallback: &str| -> Result<String, RoutingError> {
        match persisted_field(documents, key)? {
            None => Ok(fallback.to_owned()),
            Some(Value::String(value)) => Ok(value),
            Some(_) => Err(RoutingError::InvalidSelection(key)),
        }
    };
    let selection = RoutingSelection::new(
        text("runtime", base.runtime())?,
        text("provider", base.provider())?,
        text("model", base.model())?,
        match persisted_field(documents, "effort")? {
            None => base.effort().map(str::to_owned),
            Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value),
            Some(_) => return Err(RoutingError::InvalidSelection("effort")),
        },
    )?;
    let endpoint = persisted_field(documents, "endpoint")?;
    let selection = selection
        .with_endpoint(endpoint_value(endpoint.as_ref())?)?
        .with_parameters(parameters_value(
            persisted_field(documents, "parameters")?.as_ref(),
        )?)?
        .with_credential_reference(reference_value(
            persisted_field(documents, "credential_reference")?.as_ref(),
        )?);
    let selection = match persisted_field(documents, "runtime_model")? {
        None | Some(Value::Null) => selection,
        Some(Value::String(model)) => selection.with_runtime_model(model)?,
        Some(_) => return Err(RoutingError::InvalidSelection("runtime_model")),
    };
    match persisted_field(documents, "runtime_effort")? {
        None | Some(Value::Null) => Ok(selection),
        Some(Value::String(effort)) => selection.with_runtime_effort(Some(effort)),
        Some(_) => Err(RoutingError::InvalidSelection("runtime_effort")),
    }
}

/// Effective top-level runtime plus native inference route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingSelection {
    runtime: String,
    provider: String,
    model: String,
    effort: Option<String>,
    runtime_model: Option<String>,
    runtime_effort: Option<String>,
    endpoint: Option<String>,
    parameters: BTreeMap<String, String>,
    credential_reference: Option<heycode_credentials::CredentialReference>,
}

impl RoutingSelection {
    /// Explicit non-secret cloud coordinates retained with the selected route.
    #[must_use]
    pub fn parameters(&self) -> &BTreeMap<String, String> {
        &self.parameters
    }

    /// Attach bounded cloud coordinates. Provider activation validates their meaning.
    ///
    /// # Errors
    /// Unknown coordinate names or blank, oversized, padded or control-bearing values.
    pub fn with_parameters(
        mut self,
        parameters: BTreeMap<String, String>,
    ) -> Result<Self, RoutingError> {
        if parameters.iter().any(|(key, value)| {
            !matches!(
                key.as_str(),
                "region" | "project" | "location" | "resource" | "deployment"
            ) || value.is_empty()
                || value.len() > 256
                || value.trim() != value
                || value.chars().any(char::is_control)
        }) {
            return Err(RoutingError::InvalidSelection("parameters"));
        }
        self.parameters = parameters;
        Ok(self)
    }

    /// Explicit credential reference saved with the connection, never a secret value.
    #[must_use]
    pub fn credential_reference(&self) -> Option<&heycode_credentials::CredentialReference> {
        self.credential_reference.as_ref()
    }

    /// Bind a validated credential reference to this connection.
    #[must_use]
    pub fn with_credential_reference(
        mut self,
        reference: Option<heycode_credentials::CredentialReference>,
    ) -> Self {
        self.credential_reference = reference;
        self
    }

    /// Explicit endpoint selected with this connection, independent of the default.
    #[must_use]
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// Attach an explicit credential-free HTTP(S) endpoint to a route.
    ///
    /// # Errors
    /// Non-HTTP(S), invalid, oversized, credential-bearing, query or fragment URLs.
    pub fn with_endpoint(mut self, endpoint: Option<String>) -> Result<Self, RoutingError> {
        if let Some(endpoint) = endpoint.as_deref() {
            let url = url::Url::parse(endpoint)
                .map_err(|_| RoutingError::InvalidSelection("endpoint"))?;
            if endpoint.len() > 2048
                || endpoint.trim() != endpoint
                || endpoint.chars().any(char::is_control)
                || !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(RoutingError::InvalidSelection("endpoint"));
            }
        }
        self.endpoint = endpoint;
        Ok(self)
    }

    /// Construct an already-validated selection.
    ///
    /// # Errors
    /// Blank/control-bearing/oversized ids or a malformed effort value.
    pub fn new(
        runtime: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        effort: Option<String>,
    ) -> Result<Self, RoutingError> {
        let runtime = runtime.into();
        let provider = provider.into();
        let model = model.into();
        for (value, label) in [
            (runtime.as_str(), "runtime"),
            (provider.as_str(), "provider"),
            (model.as_str(), "model"),
        ] {
            if value.is_empty()
                || value.trim() != value
                || value.len() > 256
                || value.chars().any(char::is_control)
            {
                return Err(RoutingError::InvalidSelection(label));
            }
        }
        if let Some(effort) = effort.as_deref() {
            heycode_llm::ReasoningEffortId::new(effort)
                .map_err(|_| RoutingError::InvalidSelection("effort"))?;
        }
        Ok(Self {
            runtime,
            provider,
            model,
            effort,
            runtime_model: None,
            runtime_effort: None,
            endpoint: None,
            parameters: BTreeMap::new(),
            credential_reference: None,
        })
    }

    /// Replace the exact native adapter-owned effort value.
    ///
    /// # Errors
    /// Malformed values fail before the selection is returned. Whether a valid
    /// id is accepted by the active adapter is checked by [`crate::RoutingService`].
    pub fn with_effort(mut self, effort: Option<String>) -> Result<Self, RoutingError> {
        if let Some(effort) = effort.as_deref() {
            heycode_llm::ReasoningEffortId::new(effort)
                .map_err(|_| RoutingError::InvalidSelection("effort"))?;
        }
        self.effort = effort;
        Ok(self)
    }

    /// Select a delegated model independently of the native inference fallback.
    ///
    /// # Errors
    /// Native routes or blank, oversized, control-bearing model ids fail.
    pub fn with_runtime_model(mut self, model: impl Into<String>) -> Result<Self, RoutingError> {
        let model = model.into();
        if self.runtime == "native" {
            return Err(RoutingError::InvalidSelection("runtime_model"));
        }
        Self::new(&self.runtime, &self.provider, &model, None)?;
        self.runtime_model = Some(model);
        Ok(self)
    }

    /// Explicit delegated model, or the runtime-owned default when absent.
    #[must_use]
    pub fn runtime_model(&self) -> Option<&str> {
        self.runtime_model.as_deref()
    }

    /// Replace the exact delegated-runtime effort independently of the native fallback.
    ///
    /// # Errors
    /// Native routes or malformed effort ids fail before the selection is returned.
    pub fn with_runtime_effort(mut self, effort: Option<String>) -> Result<Self, RoutingError> {
        if self.runtime == "native" {
            return Err(RoutingError::InvalidSelection("runtime_effort"));
        }
        if let Some(effort) = effort.as_deref() {
            heycode_llm::ReasoningEffortId::new(effort)
                .map_err(|_| RoutingError::InvalidSelection("runtime_effort"))?;
        }
        self.runtime_effort = effort;
        Ok(self)
    }

    /// Explicit delegated reasoning effort, or the runtime-owned default when absent.
    #[must_use]
    pub fn runtime_effort(&self) -> Option<&str> {
        self.runtime_effort.as_deref()
    }

    pub(crate) fn from_value(value: &Value) -> Result<Self, RoutingError> {
        let object = value
            .as_object()
            .ok_or(RoutingError::InvalidSelection("expected an object"))?;
        let selection = Self::new(
            object
                .get("runtime")
                .and_then(Value::as_str)
                .ok_or(RoutingError::InvalidSelection("runtime"))?,
            object
                .get("provider")
                .and_then(Value::as_str)
                .ok_or(RoutingError::InvalidSelection("provider"))?,
            object
                .get("model")
                .and_then(Value::as_str)
                .ok_or(RoutingError::InvalidSelection("model"))?,
            object
                .get("effort")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )?;
        let selection = selection
            .with_endpoint(endpoint_value(object.get("endpoint"))?)?
            .with_parameters(parameters_value(object.get("parameters"))?)?
            .with_credential_reference(reference_value(object.get("credential_reference"))?);
        let selection = match object.get("runtime_model") {
            None | Some(Value::Null) => selection,
            Some(Value::String(model)) => selection.with_runtime_model(model)?,
            Some(_) => return Err(RoutingError::InvalidSelection("runtime_model")),
        };
        match object.get("runtime_effort") {
            None | Some(Value::Null) => Ok(selection),
            Some(Value::String(effort)) => selection.with_runtime_effort(Some(effort.clone())),
            Some(_) => Err(RoutingError::InvalidSelection("runtime_effort")),
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        let mut value = json!({
            "runtime": self.runtime,
            "provider": self.provider,
            "model": self.model,

        });
        value["parameters"] = json!(self.parameters);
        if let Some(reference) = &self.credential_reference {
            value["credential_reference"] = Value::String(reference.as_str().into());
        }
        if let Some(endpoint) = &self.endpoint {
            value["endpoint"] = Value::String(endpoint.clone());
        }
        if let Some(model) = &self.runtime_model
            && let Some(object) = value.as_object_mut()
        {
            object.insert("runtime_model".to_owned(), Value::String(model.clone()));
        }
        if let Some(effort) = &self.runtime_effort
            && let Some(object) = value.as_object_mut()
        {
            object.insert("runtime_effort".to_owned(), Value::String(effort.clone()));
        }
        if let Some(effort) = &self.effort
            && let Some(object) = value.as_object_mut()
        {
            object.insert("effort".to_owned(), Value::String(effort.clone()));
        }
        value
    }

    /// AgentRuntimeRegistry id.
    #[must_use]
    pub fn runtime(&self) -> &str {
        &self.runtime
    }

    /// ProviderRegistry id used inside the native loop.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Provider-native model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Adapter-owned reasoning effort, when a live route exposes one.
    #[must_use]
    pub fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }
}

/// Build the schema/base contract from the composed live registries.
///
/// # Errors
/// Static settings schema/namespace failures.
pub fn routing_definition(
    base: &RoutingSelection,
    provider_ids: BTreeSet<String>,
    runtime_ids: BTreeSet<String>,
) -> Result<SettingsDefinition, SettingsError> {
    routing_definition_with_overrides(
        base,
        &RoutingOverrides::default(),
        provider_ids,
        runtime_ids,
    )
}

/// Routing fields this process's command line pinned (`--provider`,
/// `--model`, `--set llm.*`).
///
/// They form the ephemeral [`heycode_settings::SettingsLayer::Override`] layer:
/// above the persisted user/project route for this session, below managed
/// locks, and dropped by the first in-session `/model` or `/provider`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingOverrides {
    provider: Option<String>,
    model: Option<String>,
}

impl RoutingOverrides {
    /// Pin the given fields; `None` leaves the persisted value in charge.
    #[must_use]
    pub fn new(provider: Option<String>, model: Option<String>) -> Self {
        Self { provider, model }
    }

    /// True when the command line pinned nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none()
    }

    fn to_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        if let Some(provider) = &self.provider {
            object.insert("provider".to_owned(), Value::String(provider.clone()));
        }
        if let Some(model) = &self.model {
            object.insert("model".to_owned(), Value::String(model.clone()));
        }
        Value::Object(object)
    }

    /// One line per pinned field whose persisted value differs, naming the
    /// layer it replaced, so a user who forgot a `--model` in their shell
    /// alias is told why `/model` shows something other than their settings.
    /// `None` when nothing was pinned or every pin agrees with what is stored.
    #[must_use]
    pub fn notice(&self, snapshot: &heycode_settings::SettingsSnapshot) -> Option<String> {
        let lines = [("provider", &self.provider), ("model", &self.model)]
            .into_iter()
            .filter_map(|(field, pinned)| {
                let pinned = pinned.as_deref()?;
                // Project beats user, so the project value is what the flag
                // actually displaced when both exist.
                let (layer, stored) = [("project", snapshot.project()), ("user", snapshot.user())]
                    .into_iter()
                    .find_map(|(layer, values)| {
                        values?.get(field).and_then(Value::as_str).map(|value| (layer, value))
                    })?;
                (stored != pinned).then(|| {
                    format!(
                        "command line sets {field} `{pinned}` for this session ({layer} settings have `{stored}`)"
                    )
                })
            })
            .collect::<Vec<_>>();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }
}

/// [`routing_definition`] with this process's command-line pins layered
/// above the persisted route.
///
/// # Errors
/// Same as [`routing_definition`].
pub fn routing_definition_with_overrides(
    base: &RoutingSelection,
    overrides: &RoutingOverrides,
    provider_ids: BTreeSet<String>,
    runtime_ids: BTreeSet<String>,
) -> Result<SettingsDefinition, SettingsError> {
    let schema = SettingsSchema::new(
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "runtime": {"type": "string"},
                "provider": {"type": "string"},
                "model": {"type": "string"},
                "effort": {"type": ["string", "null"]},
                "runtime_model": {"type": ["string", "null"]},
                "runtime_effort": {"type": ["string", "null"]},
                "endpoint": {"type": ["string", "null"]},
                "credential_reference": {"type": ["string", "null"]},
                "parameters": {"type": "object", "additionalProperties": {"type": "string"}},
                "setup_required": {"type": "boolean"},
                "pending_connection": {"type": ["object", "null"], "additionalProperties": false,
                    "properties": {"parameters": {"type":"object", "additionalProperties":{"type":"string"}}, "provider": {"type":"string"}, "model": {"type":"string"}, "endpoint": {"type":["string", "null"]}, "credential_reference": {"type":["string", "null"]}},
                    "required": ["provider", "model"]}
            }
        }),
        json!({
            "runtime": "",
            "provider": "",
            "model": "",
            "effort": null,
            "runtime_model": null,
            "runtime_effort": null,
            "setup_required": false,
            "pending_connection": null,
            "endpoint": null,
            "credential_reference": null,
            "parameters": {}
        }),
        move |value| validate_route_value(value, &provider_ids, &runtime_ids),
    )?
    .with_public_path(heycode_settings::SettingsFieldPath::new("credential_reference")?)
    .with_public_path(heycode_settings::SettingsFieldPath::new("pending_connection.credential_reference")?)
    .with_wire_exposure();
    let definition =
        SettingsDefinition::new(settings_namespace()?, schema).with_base(base.to_value())?;
    if overrides.is_empty() {
        return Ok(definition);
    }
    definition.with_override(overrides.to_value())
}

fn validate_route_value(
    value: &Value,
    provider_ids: &BTreeSet<String>,
    runtime_ids: &BTreeSet<String>,
) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "routing settings must be an object".to_owned())?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "runtime"
                | "provider"
                | "model"
                | "effort"
                | "runtime_model"
                | "runtime_effort"
                | "pending_connection"
                | "endpoint"
                | "credential_reference"
                | "parameters"
                | "setup_required"
        )
    }) {
        return Err("routing settings contain an unknown field".to_owned());
    }
    let runtime = object.get("runtime").and_then(Value::as_str).unwrap_or("");
    let provider = object.get("provider").and_then(Value::as_str).unwrap_or("");
    let model = object.get("model").and_then(Value::as_str).unwrap_or("");
    for (label, value) in [
        ("runtime", runtime),
        ("provider", provider),
        ("model", model),
    ] {
        if value.len() > 256 || value.chars().any(char::is_control) {
            return Err(format!("routing {label} is invalid"));
        }
    }
    let empty_count = [runtime, provider, model]
        .into_iter()
        .filter(|value| value.is_empty())
        .count();
    if empty_count != 0 && empty_count != 3 {
        return Err("routing runtime/provider/model must be set together".to_owned());
    }
    if object
        .get("setup_required")
        .is_some_and(|required| !required.is_boolean())
    {
        return Err("routing setup_required must be boolean".to_owned());
    }
    if !runtime.is_empty() && !runtime_ids.contains(runtime) {
        return Err("routing runtime is not registered".to_owned());
    }
    if !provider.is_empty() && !provider_ids.contains(provider) {
        return Err("routing provider is not registered".to_owned());
    }
    if let Some(pending) =
        pending_connection(object.get("pending_connection")).map_err(|error| error.to_string())?
        && !provider_ids.contains(pending.provider())
    {
        return Err("pending connection provider is not registered".into());
    }
    if let Some(model) = object.get("runtime_model").filter(|value| !value.is_null()) {
        let model = model.as_str().ok_or("routing runtime_model must be text")?;
        RoutingSelection::new(runtime, provider, model, None)
            .and_then(|selection| selection.with_runtime_model(model))
            .map_err(|_| "routing runtime_model is invalid".to_owned())?;
    }
    if let Some(effort) = object
        .get("runtime_effort")
        .filter(|value| !value.is_null())
    {
        let effort = effort
            .as_str()
            .ok_or("routing runtime_effort must be text")?;
        RoutingSelection::new(runtime, provider, model, None)
            .and_then(|selection| selection.with_runtime_effort(Some(effort.to_owned())))
            .map_err(|_| "routing runtime_effort is invalid".to_owned())?;
    }
    reference_value(object.get("credential_reference")).map_err(|error| error.to_string())?;
    RoutingSelection::new("native", "endpoint-validation", "endpoint-validation", None)
        .and_then(|route| route.with_endpoint(endpoint_value(object.get("endpoint"))?))
        .and_then(|route| route.with_parameters(parameters_value(object.get("parameters"))?))
        .map_err(|error| error.to_string())?;
    if let Some(effort) = object.get("effort").filter(|effort| !effort.is_null()) {
        let effort = effort.as_str().ok_or("routing effort must be text")?;
        heycode_llm::ReasoningEffortId::new(effort)
            .map_err(|_| "routing effort is invalid".to_owned())?;
    }
    Ok(())
}
