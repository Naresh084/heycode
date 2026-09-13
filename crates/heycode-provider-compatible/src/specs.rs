use heycode_core::ProviderProtocol;
use heycode_llm::{ProviderDescriptor, ProviderProfile};

/// Exact model-list response dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogDialect {
    /// OpenAI's `data` array, with Groq's maintained intrinsic evidence.
    Groq,
    /// Fireworks account model metadata and bounded pagination.
    Fireworks,
    /// Mistral model cards with explicit chat and intrinsic capability evidence.
    Mistral,
    /// Together's task-typed model array.
    Together,
    /// xAI's language-only model envelope and modalities.
    Xai,
}

/// One provider-owned endpoint, credential reference and discovery contract.
#[derive(Debug, Clone, Copy)]
pub struct CompatibleSpec {
    /// Stable provider identity.
    pub id: &'static str,
    /// Human connection name.
    pub name: &'static str,
    /// Documented Chat Completions version root.
    pub base_url: &'static str,
    /// Documented model-list endpoint.
    pub models_url: &'static str,
    /// Non-secret credential reference.
    pub credential_reference: &'static str,
    /// Explicit fallback when discovery is unavailable.
    pub default_model: &'static str,
    /// Exact discovery response shape.
    pub catalog_dialect: CatalogDialect,
}

impl CompatibleSpec {
    /// Protocol identity; no model capability is inferred here.
    #[must_use]
    pub fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: self.id.into(),
            display_name: self.name.into(),
            protocols: vec![ProviderProtocol::OpenAiChatCompletions],
        }
    }

    /// Catalog endpoint corresponding to an optional explicit inference override.
    /// Custom endpoints stay on the supplied authority for credential isolation.
    #[must_use]
    pub fn models_endpoint(&self, base_url: Option<&str>) -> String {
        match base_url {
            None => self.models_url.into(),
            Some(base) => {
                let base = base.trim_end_matches('/');
                if self.catalog_dialect == CatalogDialect::Xai {
                    return format!("{base}/language-models");
                }
                if self.catalog_dialect == CatalogDialect::Fireworks
                    && let Some(origin) = base.strip_suffix("/inference/v1")
                {
                    format!("{origin}/v1/accounts/fireworks/models")
                } else {
                    format!("{base}/models")
                }
            }
        }
    }

    /// Safe setup metadata independent of an active inference client.
    #[must_use]
    pub fn profile(&self) -> ProviderProfile {
        ProviderProfile {
            registry_name: self.id.into(),
            descriptor: self.descriptor(),
            default_model: self.default_model.into(),
            credential_reference: Some(self.credential_reference.into()),
        }
    }
}

/// Reviewed provider bindings. Model availability is refreshed at connection time.
#[must_use]
pub fn builtin_specs() -> &'static [CompatibleSpec] {
    &[
        CompatibleSpec {
            id: "xai",
            name: "xAI",
            base_url: "https://api.x.ai/v1",
            models_url: "https://api.x.ai/v1/language-models",
            credential_reference: "XAI_API_KEY",
            default_model: "grok-4.6",
            catalog_dialect: CatalogDialect::Xai,
        },
        CompatibleSpec {
            id: "together",
            name: "Together AI",
            base_url: "https://api.together.ai/v1",
            models_url: "https://api.together.ai/v1/models",
            credential_reference: "TOGETHER_API_KEY",
            default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            catalog_dialect: CatalogDialect::Together,
        },
        CompatibleSpec {
            id: "mistral",
            name: "Mistral AI",
            base_url: "https://api.mistral.ai/v1",
            models_url: "https://api.mistral.ai/v1/models",
            credential_reference: "MISTRAL_API_KEY",
            default_model: "mistral-small-latest",
            catalog_dialect: CatalogDialect::Mistral,
        },
        CompatibleSpec {
            id: "fireworks",
            name: "Fireworks AI",
            base_url: "https://api.fireworks.ai/inference/v1",
            models_url: "https://api.fireworks.ai/v1/accounts/fireworks/models",
            credential_reference: "FIREWORKS_API_KEY",
            default_model: "accounts/fireworks/models/kimi-k2-instruct-0905",
            catalog_dialect: CatalogDialect::Fireworks,
        },
        CompatibleSpec {
            id: "groq",
            name: "Groq",
            base_url: "https://api.groq.com/openai/v1",
            models_url: "https://api.groq.com/openai/v1/models",
            credential_reference: "GROQ_API_KEY",
            default_model: "openai/gpt-oss-120b",
            catalog_dialect: CatalogDialect::Groq,
        },
    ]
}

/// Look up a maintained compatible provider by its exact identity.
#[must_use]
pub fn spec(id: &str) -> Option<&'static CompatibleSpec> {
    builtin_specs().iter().find(|row| row.id == id)
}
