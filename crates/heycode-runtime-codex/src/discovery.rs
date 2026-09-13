//! Pinned 0.153.2 account/model/capability response normalization.

use std::collections::{BTreeSet, HashSet};

use heycode_core::ProviderProtocol;
use heycode_llm::{
    CapabilitySupport, CatalogSnapshot, ModelCapabilities, ModelDescriptor, ModelLifecycle,
    ProviderDescriptor,
};
use heycode_runtime::{AccountState, AccountStatus};
use serde_json::{Map, Value};

use crate::{CodexAppServerError, CodexAppServerErrorCode, CodexResponsePayload};

pub(crate) const MODEL_PAGE_LIMIT: u32 = 100;
pub(crate) const MAX_MODEL_PAGES: usize = 64;
pub(crate) const MAX_MODELS: usize = 4_096;

const MAX_ID_BYTES: usize = 256;
const MAX_DISPLAY_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 1_024;
const MAX_CURSOR_BYTES: usize = 1_024;
const MAX_REASONING_EFFORTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderCapabilities {
    web_search: bool,
    image_generation: bool,
    namespace_tools: bool,
}

impl ProviderCapabilities {
    pub(crate) fn parse(payload: CodexResponsePayload) -> Result<Self, CodexAppServerError> {
        let value = payload.into_value();
        let object = value.as_object().ok_or_else(protocol)?;
        Ok(Self {
            web_search: required_bool(object, "webSearch")?,
            image_generation: required_bool(object, "imageGeneration")?,
            namespace_tools: required_bool(object, "namespaceTools")?,
        })
    }
}

pub(crate) fn parse_account(
    payload: CodexResponsePayload,
) -> Result<AccountState, CodexAppServerError> {
    let value = payload.into_value();
    let object = value.as_object().ok_or_else(protocol)?;
    let requires_openai_auth = required_bool(object, "requiresOpenaiAuth")?;
    let account = object.get("account");
    if account.is_none_or(Value::is_null) {
        return Ok(AccountState::without_label(if requires_openai_auth {
            AccountStatus::Disconnected
        } else {
            AccountStatus::NotRequired
        }));
    }
    let account = account.and_then(Value::as_object).ok_or_else(protocol)?;
    match required_text(account, "type", 64)? {
        "apiKey" => AccountState::connected(Some("OpenAI API key")).map_err(|_| protocol()),
        "chatgpt" => {
            match account.get("email") {
                Some(Value::Null) => {}
                Some(Value::String(email)) if valid_text(email, 254) => {}
                _ => return Err(protocol()),
            }
            let plan = required_text(account, "planType", 64)?;
            if !matches!(
                plan,
                "free"
                    | "go"
                    | "plus"
                    | "pro"
                    | "prolite"
                    | "team"
                    | "self_serve_business_prolite"
                    | "self_serve_business_usage_based"
                    | "business"
                    | "ent26"
                    | "enterprise_cbp_automation"
                    | "enterprise_cbp_usage_based"
                    | "enterprise"
                    | "edu"
                    | "edu_plus"
                    | "edu_pro"
                    | "unknown"
            ) {
                return Err(protocol());
            }
            let label = format!("ChatGPT {plan}");
            AccountState::connected(Some(&label)).map_err(|_| protocol())
        }
        "amazonBedrock" => {
            let managed = match account.get("usesCodexManagedCredentials") {
                None => false,
                Some(Value::Bool(value)) => *value,
                Some(_) => return Err(protocol()),
            };
            let label = if managed {
                "Amazon Bedrock (Codex managed)"
            } else {
                "Amazon Bedrock (AWS managed)"
            };
            AccountState::connected(Some(label)).map_err(|_| protocol())
        }
        _ => Err(protocol()),
    }
}

pub(crate) struct ModelPage {
    pub(crate) models: Vec<RawModel>,
    pub(crate) next_cursor: Option<String>,
}

pub(crate) struct RawModel {
    id: String,
    model: String,
    display_name: String,
    description: String,
    hidden: bool,
    default_reasoning_effort: String,
    supported_reasoning_efforts: Vec<String>,
    input_modalities: BTreeSet<String>,
    upgrade: Option<String>,
    is_default: bool,
}

impl ModelPage {
    pub(crate) fn parse(payload: CodexResponsePayload) -> Result<Self, CodexAppServerError> {
        let value = payload.into_value();
        let object = value.as_object().ok_or_else(protocol)?;
        let data = object
            .get("data")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() <= usize::try_from(MODEL_PAGE_LIMIT).unwrap_or(usize::MAX))
            .ok_or_else(protocol)?;
        let models = data
            .iter()
            .map(RawModel::parse)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = match object.get("nextCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(cursor)) if valid_text(cursor, MAX_CURSOR_BYTES) => {
                Some(cursor.clone())
            }
            _ => return Err(protocol()),
        };
        Ok(Self {
            models,
            next_cursor,
        })
    }
}

impl RawModel {
    fn parse(value: &Value) -> Result<Self, CodexAppServerError> {
        let object = value.as_object().ok_or_else(protocol)?;
        let id = required_owned(object, "id", MAX_ID_BYTES)?;
        let model = required_owned(object, "model", MAX_ID_BYTES)?;
        let display_name = required_owned(object, "displayName", MAX_DISPLAY_BYTES)?;
        let description = required_owned(object, "description", MAX_DESCRIPTION_BYTES)?;
        let hidden = required_bool(object, "hidden")?;
        let is_default = required_bool(object, "isDefault")?;
        let default_reasoning_effort =
            required_owned(object, "defaultReasoningEffort", MAX_ID_BYTES)?;
        let effort_rows = object
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() <= MAX_REASONING_EFFORTS)
            .ok_or_else(protocol)?;
        let mut efforts = Vec::with_capacity(effort_rows.len());
        let mut seen_efforts = HashSet::new();
        for row in effort_rows {
            let row = row.as_object().ok_or_else(protocol)?;
            let effort = required_owned(row, "reasoningEffort", MAX_ID_BYTES)?;
            let _description = required_text(row, "description", MAX_DESCRIPTION_BYTES)?;
            if !seen_efforts.insert(effort.clone()) {
                return Err(protocol());
            }
            efforts.push(effort);
        }
        if !efforts.is_empty() && !efforts.contains(&default_reasoning_effort) {
            return Err(protocol());
        }
        let modality_values = match object.get("inputModalities") {
            None => vec![
                Value::String("text".to_owned()),
                Value::String("image".to_owned()),
            ],
            Some(Value::Array(values)) if values.len() <= 3 => values.clone(),
            _ => return Err(protocol()),
        };
        let mut input_modalities = BTreeSet::new();
        for value in modality_values {
            let modality = value.as_str().ok_or_else(protocol)?;
            if !matches!(modality, "text" | "image" | "audio")
                || !input_modalities.insert(modality.to_owned())
            {
                return Err(protocol());
            }
        }
        if !input_modalities.contains("text") {
            return Err(protocol());
        }
        let upgrade = match object.get("upgrade") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if valid_text(value, MAX_ID_BYTES) => Some(value.clone()),
            _ => return Err(protocol()),
        };
        Ok(Self {
            id,
            model,
            display_name,
            description,
            hidden,
            default_reasoning_effort,
            supported_reasoning_efforts: efforts,
            input_modalities,
            upgrade,
            is_default,
        })
    }

    fn normalize(self, provider: ProviderCapabilities) -> Option<ModelDescriptor> {
        if self.hidden {
            return None;
        }
        let aliases = (self.model != self.id)
            .then_some(self.model)
            .into_iter()
            .collect();
        let reasoning = if self.supported_reasoning_efforts.is_empty() {
            CapabilitySupport::Unsupported
        } else {
            CapabilitySupport::Supported
        };
        let _validated_default_effort = self.default_reasoning_effort;
        let lifecycle = self
            .upgrade
            .map_or_else(ModelLifecycle::unknown, |upgrade| {
                ModelLifecycle::deprecated(None, vec![upgrade])
            });
        Some(ModelDescriptor {
            // The pinned app-server model catalog publishes no price or
            // performance evidence.
            pricing: heycode_llm::ModelPricing::unknown(),
            performance: heycode_llm::ModelPerformance::unknown(),
            id: self.id,
            display_name: self.display_name,
            aliases,
            created_at_ms: None,
            context_window: None,
            max_output_tokens: None,
            lifecycle,
            capabilities: ModelCapabilities {
                tools: if provider.namespace_tools {
                    CapabilitySupport::Supported
                } else {
                    CapabilitySupport::Unknown
                },
                reasoning,
                image_input: if self.input_modalities.contains("image") {
                    CapabilitySupport::Supported
                } else {
                    CapabilitySupport::Unsupported
                },
                document_input: CapabilitySupport::Unknown,
                structured_output: CapabilitySupport::Unknown,
                native_web: if provider.web_search {
                    CapabilitySupport::Supported
                } else {
                    CapabilitySupport::Unsupported
                },
                native_compaction: CapabilitySupport::Unknown,
                prompt_cache: CapabilitySupport::Unknown,
            },
            reasoning: None,
        })
    }

    pub(crate) fn configuration(&self) -> Option<heycode_runtime::RuntimeModelConfiguration> {
        (!self.hidden).then(|| heycode_runtime::RuntimeModelConfiguration {
            model: self.id.clone(),
            display_name: self.display_name.clone(),
            resolved_model: (self.model != self.id).then(|| self.model.clone()),
            description: Some(self.description.clone()),
            context_window: None,
            default_reasoning_effort: (!self.default_reasoning_effort.is_empty())
                .then(|| self.default_reasoning_effort.clone()),
            reasoning_efforts: self.supported_reasoning_efforts.clone(),
        })
    }
}

pub(crate) fn finish_catalog(
    raw_models: Vec<RawModel>,
    provider_capabilities: ProviderCapabilities,
    fetched_at_ms: u64,
) -> Result<CatalogSnapshot, CodexAppServerError> {
    if raw_models.len() > MAX_MODELS {
        return Err(protocol());
    }
    let defaults = raw_models.iter().filter(|model| model.is_default).count();
    if defaults > 1 {
        return Err(protocol());
    }
    let mut models = raw_models
        .into_iter()
        .filter_map(|model| model.normalize(provider_capabilities))
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    let mut routes = HashSet::new();
    for model in &models {
        if !routes.insert(model.id.clone()) {
            return Err(protocol());
        }
        for alias in &model.aliases {
            if !routes.insert(alias.clone()) {
                return Err(protocol());
            }
        }
    }
    Ok(CatalogSnapshot {
        provider: ProviderDescriptor {
            id: "codex".to_owned(),
            display_name: "Codex".to_owned(),
            protocols: vec![ProviderProtocol::DelegatedAgent],
        },
        models,
        revision: 1,
        fetched_at_ms,
    })
}

fn required_bool(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<bool, CodexAppServerError> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(protocol)
}

fn required_owned(
    object: &Map<String, Value>,
    field: &'static str,
    max_bytes: usize,
) -> Result<String, CodexAppServerError> {
    required_text(object, field, max_bytes).map(str::to_owned)
}

fn required_text<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
    max_bytes: usize,
) -> Result<&'a str, CodexAppServerError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| valid_text(value, max_bytes))
        .ok_or_else(protocol)
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn protocol() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::Protocol)
}
