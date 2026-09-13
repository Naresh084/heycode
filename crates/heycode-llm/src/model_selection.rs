//! Deterministic model selection against one committed catalog generation.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::{CatalogError, CatalogSnapshot, ModelDescriptor, ModelLifecycleStatus};

const ALTERNATIVE_LIMIT: usize = 5;

/// Non-fatal lifecycle warning returned with a valid configured model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSelectionWarning {
    /// The model remains selectable but is deprecated.
    Deprecated {
        /// Exact retirement instant in Unix milliseconds when known.
        retirement_at_ms: Option<u64>,
        /// Available replacement ids in deterministic recommendation order.
        alternatives: Vec<String>,
    },
}

/// Catalog-backed configured model selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelSelection {
    /// Exact descriptor selected from the committed generation.
    pub descriptor: ModelDescriptor,
    /// Non-fatal lifecycle warning, when applicable.
    pub warning: Option<ModelSelectionWarning>,
}

/// Failures resolving a configured provider/model reference.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelSelectionError {
    /// Catalog registry/cache access failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The configured model id is absent from the committed provider catalog.
    #[error(
        "configured model `{model}` is absent from provider `{provider}`; alternatives: {alternatives:?}"
    )]
    UnknownModel {
        /// Provider id.
        provider: String,
        /// Configured model id.
        model: String,
        /// Available alternatives in deterministic recommendation order.
        alternatives: Vec<String>,
    },
    /// The configured model is explicitly or effectively retired.
    #[error(
        "configured model `{model}` for provider `{provider}` is retired; alternatives: {alternatives:?}"
    )]
    RetiredModel {
        /// Provider id.
        provider: String,
        /// Configured model id.
        model: String,
        /// Exact retirement instant in Unix milliseconds when known.
        retirement_at_ms: Option<u64>,
        /// Available alternatives in deterministic recommendation order.
        alternatives: Vec<String>,
    },
}

impl CatalogSnapshot {
    /// Resolve one configured model against this exact catalog generation.
    ///
    /// Unknown ids and effectively retired models fail with bounded,
    /// deterministic alternatives. Deprecated models remain selectable with
    /// an explicit warning.
    ///
    /// # Errors
    /// [`ModelSelectionError::UnknownModel`] or
    /// [`ModelSelectionError::RetiredModel`] when dispatch must not proceed.
    pub fn resolve_model(
        &self,
        model: &str,
        at_ms: u64,
    ) -> Result<ResolvedModelSelection, ModelSelectionError> {
        let descriptor = self.models.iter().find(|candidate| {
            candidate.id == model || candidate.aliases.iter().any(|alias| alias == model)
        });
        let Some(descriptor) = descriptor else {
            return Err(ModelSelectionError::UnknownModel {
                provider: self.provider.id.clone(),
                model: model.to_owned(),
                alternatives: alternatives(self, model, &[], at_ms),
            });
        };
        let status = descriptor.lifecycle.effective_status(at_ms);
        if status == ModelLifecycleStatus::Retired {
            return Err(ModelSelectionError::RetiredModel {
                provider: self.provider.id.clone(),
                model: model.to_owned(),
                retirement_at_ms: descriptor.lifecycle.retirement_at_ms,
                alternatives: alternatives(
                    self,
                    &descriptor.id,
                    &descriptor.lifecycle.replacement_ids,
                    at_ms,
                ),
            });
        }
        let warning = (status == ModelLifecycleStatus::Deprecated).then(|| {
            ModelSelectionWarning::Deprecated {
                retirement_at_ms: descriptor.lifecycle.retirement_at_ms,
                alternatives: alternatives(
                    self,
                    &descriptor.id,
                    &descriptor.lifecycle.replacement_ids,
                    at_ms,
                ),
            }
        });
        Ok(ResolvedModelSelection {
            descriptor: descriptor.clone(),
            warning,
        })
    }
}

fn alternatives(
    snapshot: &CatalogSnapshot,
    rejected: &str,
    preferred: &[String],
    at_ms: u64,
) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for id in preferred {
        if let Some(candidate) = selectable(snapshot, id, rejected, at_ms)
            && seen.insert(candidate.id.as_str())
        {
            result.push(candidate.id.clone());
            if result.len() == ALTERNATIVE_LIMIT {
                return result;
            }
        }
    }
    for status in [
        ModelLifecycleStatus::Stable,
        ModelLifecycleStatus::Preview,
        ModelLifecycleStatus::Unknown,
        ModelLifecycleStatus::Deprecated,
    ] {
        for candidate in &snapshot.models {
            if candidate.id != rejected
                && candidate.lifecycle.effective_status(at_ms) == status
                && seen.insert(candidate.id.as_str())
            {
                result.push(candidate.id.clone());
                if result.len() == ALTERNATIVE_LIMIT {
                    return result;
                }
            }
        }
    }
    result
}

fn selectable<'a>(
    snapshot: &'a CatalogSnapshot,
    id: &str,
    rejected: &str,
    at_ms: u64,
) -> Option<&'a ModelDescriptor> {
    snapshot
        .models
        .iter()
        .find(|candidate| {
            candidate.id != rejected
                && (candidate.id == id || candidate.aliases.iter().any(|alias| alias == id))
        })
        .filter(|candidate| candidate.lifecycle.is_selectable(at_ms))
}
