//! Non-secret credential-reference settings namespace.

use heycode_settings::{SettingsDefinition, SettingsError, SettingsNamespace, SettingsSchema};
use serde_json::{Value, json};

use crate::CredentialReference;

/// Credential reference settings namespace.
///
/// # Errors
/// The built-in literal is validated through the same public boundary.
pub fn settings_namespace() -> Result<SettingsNamespace, SettingsError> {
    SettingsNamespace::new("credentials")
}

pub(crate) fn definition() -> Result<SettingsDefinition, SettingsError> {
    let namespace = settings_namespace()?;
    let schema = SettingsSchema::new(
        json!({
            "type": "object",
            "properties": {
                "references": {
                    "type": "object",
                    "additionalProperties": {"type": "string"}
                }
            }
        }),
        json!({"references": {}}),
        validate,
    )?
    .with_wire_exposure();
    Ok(SettingsDefinition::new(namespace, schema))
}

fn validate(value: &Value) -> Result<(), String> {
    let references = value
        .get("references")
        .and_then(Value::as_object)
        .ok_or_else(|| "references must be an object".to_owned())?;
    for (owner, reference) in references {
        let raw = reference
            .as_str()
            .ok_or_else(|| format!("reference `{owner}` must be a string"))?;
        CredentialReference::new(raw).map_err(|error| error.to_string())?;
    }
    Ok(())
}
