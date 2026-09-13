//! Layered user catalog overrides and the attribution they produce.
//!
//! An override is an **assertion**, not evidence. This module is the only mint
//! for [`OverrideSource`], and it mints one solely from the layer whose bytes
//! it is reading, so every value that leaves here as a user assertion is
//! labelled with the document that made it. Nothing downstream can strip that
//! label, because [`crate::AttributedSupport`] has no un-labelled variant.
//!
//! Applying overrides never mutates a [`CatalogSnapshot`]. Attribution is a
//! projection over a borrowed generation, so the durable catalog cache written
//! by [`crate::FileCatalogPersistence`] continues to hold provider evidence and
//! nothing else — a user assertion has no path into it and therefore cannot be
//! read back on the next start as though a provider had published it.

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use heycode_llm::{CapabilitySupport, CatalogSnapshot};

use crate::attribution::{
    AttributedCatalog, AttributedLimit, AttributedModel, AttributedSupport, ModelCapabilityKind,
    ModelLimitField, OverrideSource, UnmatchedOverride,
};
use crate::error::CatalogOverrideError;
use crate::override_wire::{WireOverrideDocument, WireOverrideProbe};

/// Current on-disk catalog-override schema.
pub const OVERRIDE_SCHEMA_VERSION: u32 = 1;

const MAX_OVERRIDE_BYTES: u64 = 1024 * 1024;

/// One override layer: a caller-assigned label and the exact document path.
///
/// The label is what a surface shows a user when it says where an assertion
/// came from, so it is required and must be unique across the configured
/// layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogOverrideLayer {
    label: String,
    path: PathBuf,
}

impl CatalogOverrideLayer {
    /// Configure one labelled override document.
    #[must_use]
    pub fn new(label: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
        }
    }

    /// Layer label shown to a user.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Exact document path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Ordered override layers, lowest precedence first.
///
/// Precedence is resolved **per field**: a higher layer that asserts `tools`
/// does not erase a lower layer's `prompt_cache`, and each surviving field
/// names the exact layer that supplied it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogOverridesConfig {
    layers: Vec<CatalogOverrideLayer>,
}

impl CatalogOverridesConfig {
    /// Configure layers in ascending precedence order.
    #[must_use]
    pub fn new(layers: Vec<CatalogOverrideLayer>) -> Self {
        Self { layers }
    }

    /// Append one higher-precedence layer.
    #[must_use]
    pub fn with_layer(mut self, layer: CatalogOverrideLayer) -> Self {
        self.layers.push(layer);
        self
    }

    /// Configured layers in ascending precedence order.
    #[must_use]
    pub fn layers(&self) -> &[CatalogOverrideLayer] {
        &self.layers
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ModelOverride {
    capabilities: [Option<(CapabilitySupport, OverrideSource)>; ModelCapabilityKind::COUNT],
    context_window: Option<(NonZeroU64, OverrideSource)>,
    max_output_tokens: Option<(NonZeroU64, OverrideSource)>,
}

impl ModelOverride {
    fn is_empty(&self) -> bool {
        self.context_window.is_none()
            && self.max_output_tokens.is_none()
            && self.capabilities.iter().all(Option::is_none)
    }

    /// Every distinct document that contributed a field to this entry, in
    /// stable field order. An inert override is reported once per document, so
    /// no layer that named a nonexistent model is left unmentioned.
    fn sources(&self) -> Vec<&OverrideSource> {
        let mut sources: Vec<&OverrideSource> = Vec::new();
        for source in self
            .capabilities
            .iter()
            .flatten()
            .map(|(_, source)| source)
            .chain(self.context_window.iter().map(|(_, source)| source))
            .chain(self.max_output_tokens.iter().map(|(_, source)| source))
        {
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
        sources
    }
}

/// Loaded user assertions from every configured layer.
///
/// Values are keyed by `(provider id, canonical model id)`. An override matches
/// a catalog row by canonical id only; an id that exists solely as a provider
/// alias is reported through [`AttributedCatalog::unmatched`] rather than
/// resolved, so one row can never collect two conflicting entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogOverrides {
    entries: BTreeMap<String, BTreeMap<String, ModelOverride>>,
    captured_at_ms: Option<u64>,
}

impl CatalogOverrides {
    /// No assertions at all.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Read every configured layer in ascending precedence order.
    ///
    /// A missing document is an empty layer, not a failure: overrides are
    /// optional. Anything else — an unsafe path, an unsupported schema, an
    /// unrecognized capability word, a duplicate or empty entry — fails the
    /// whole load rather than applying part of a user's intent.
    ///
    /// # Errors
    /// Unsafe paths, unsupported schemas, malformed documents, duplicate or
    /// blank layer labels, and invalid entries.
    pub fn load(config: &CatalogOverridesConfig) -> Result<Self, CatalogOverrideError> {
        let captured_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| duration.as_millis().try_into().ok())
            .filter(|captured_at_ms| *captured_at_ms != 0)
            .ok_or(CatalogOverrideError::ClockUnavailable)?;
        Self::load_at(config, captured_at_ms)
    }

    /// Read one immutable override generation at a host-supplied capture
    /// instant.
    ///
    /// This is the deterministic constructor for hosts/tests that already own
    /// a trusted clock. Layer precedence is the configured low-to-high index;
    /// the index, source path and this instant are retained on every assertion.
    /// Changing a source file cannot mutate the returned generation; callers
    /// must explicitly load another generation.
    ///
    /// # Errors
    /// Same contract as [`Self::load`], plus
    /// [`CatalogOverrideError::MissingCaptureInstant`] for zero.
    pub fn load_at(
        config: &CatalogOverridesConfig,
        captured_at_ms: u64,
    ) -> Result<Self, CatalogOverrideError> {
        if captured_at_ms == 0 {
            return Err(CatalogOverrideError::MissingCaptureInstant);
        }
        let mut seen_labels: Vec<&str> = Vec::new();
        for layer in config.layers() {
            let label = layer.label().trim();
            if label.is_empty() {
                return Err(CatalogOverrideError::BlankLayerLabel);
            }
            if seen_labels.contains(&label) {
                return Err(CatalogOverrideError::DuplicateLayerLabel {
                    label: label.to_owned(),
                });
            }
            seen_labels.push(label);
        }

        let mut overrides = Self {
            entries: BTreeMap::new(),
            captured_at_ms: Some(captured_at_ms),
        };
        for (precedence, layer) in config.layers().iter().enumerate() {
            overrides.apply_layer(layer, precedence, captured_at_ms)?;
        }
        Ok(overrides)
    }

    /// Whether any layer asserted anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of `(provider, model)` rows carrying at least one assertion.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.values().map(BTreeMap::len).sum()
    }

    /// Capture instant of this immutable loaded generation.
    ///
    /// [`Self::empty`] has none because it was not read from any document.
    #[must_use]
    pub const fn captured_at_ms(&self) -> Option<u64> {
        self.captured_at_ms
    }

    fn apply_layer(
        &mut self,
        layer: &CatalogOverrideLayer,
        precedence: usize,
        captured_at_ms: u64,
    ) -> Result<(), CatalogOverrideError> {
        let Some(raw) = read_layer(layer.path())? else {
            return Ok(());
        };
        let probe: WireOverrideProbe = toml::from_str(&raw)
            .map_err(|error| CatalogOverrideError::parse(layer.path(), error.to_string()))?;
        let found = probe.schema_version.unwrap_or(0);
        if found > OVERRIDE_SCHEMA_VERSION {
            return Err(CatalogOverrideError::NewerSchema {
                path: layer.path().display().to_string(),
                found,
                supported: OVERRIDE_SCHEMA_VERSION,
            });
        }
        if found < OVERRIDE_SCHEMA_VERSION {
            return Err(CatalogOverrideError::OlderSchema {
                path: layer.path().display().to_string(),
                found,
                supported: OVERRIDE_SCHEMA_VERSION,
            });
        }
        let document: WireOverrideDocument = toml::from_str(&raw)
            .map_err(|error| CatalogOverrideError::parse(layer.path(), error.to_string()))?;

        let path_label = layer.path().display().to_string();
        let mut declared: Vec<(String, String)> = Vec::new();
        for entry in document.models {
            let provider = entry.provider.trim();
            if provider.is_empty() {
                return Err(CatalogOverrideError::BlankIdentity {
                    path: path_label.clone(),
                    field: "provider",
                });
            }
            let model = entry.model.trim();
            if model.is_empty() {
                return Err(CatalogOverrideError::BlankIdentity {
                    path: path_label.clone(),
                    field: "model",
                });
            }
            let key = (provider.to_owned(), model.to_owned());
            if declared.contains(&key) {
                return Err(CatalogOverrideError::DuplicateModel {
                    path: path_label.clone(),
                    provider: key.0,
                    model: key.1,
                });
            }
            declared.push(key.clone());
            let (provider, model) = key;

            // The source is minted here, from the layer whose bytes produced
            // this entry. It is never read out of the document.
            let source = OverrideSource::new(
                layer.label(),
                path_label.clone(),
                precedence,
                captured_at_ms,
            );
            let mut parsed = ModelOverride::default();
            for kind in ModelCapabilityKind::ALL {
                let Some(value) = entry.capabilities.value(kind) else {
                    continue;
                };
                let support =
                    parse_support(value).ok_or_else(|| CatalogOverrideError::UnknownSupport {
                        path: path_label.clone(),
                        capability: kind.name(),
                        provider: provider.clone(),
                        model: model.clone(),
                        value: value.to_owned(),
                    })?;
                parsed.capabilities[kind.index()] = Some((support, source.clone()));
            }
            for (field, raw_limit) in [
                (ModelLimitField::ContextWindow, entry.context_window),
                (ModelLimitField::MaxOutputTokens, entry.max_output_tokens),
            ] {
                let Some(raw_limit) = raw_limit else {
                    continue;
                };
                let limit =
                    NonZeroU64::new(raw_limit).ok_or_else(|| CatalogOverrideError::ZeroLimit {
                        path: path_label.clone(),
                        field: field.name(),
                        provider: provider.clone(),
                        model: model.clone(),
                    })?;
                match field {
                    ModelLimitField::ContextWindow => {
                        parsed.context_window = Some((limit, source.clone()));
                    }
                    ModelLimitField::MaxOutputTokens => {
                        parsed.max_output_tokens = Some((limit, source.clone()));
                    }
                }
            }
            if parsed.is_empty() {
                return Err(CatalogOverrideError::EmptyOverride {
                    path: path_label.clone(),
                    provider,
                    model,
                });
            }

            let target = self
                .entries
                .entry(provider)
                .or_default()
                .entry(model)
                .or_default();
            for kind in ModelCapabilityKind::ALL {
                if let Some(asserted) = parsed.capabilities[kind.index()].take() {
                    target.capabilities[kind.index()] = Some(asserted);
                }
            }
            if let Some(asserted) = parsed.context_window.take() {
                target.context_window = Some(asserted);
            }
            if let Some(asserted) = parsed.max_output_tokens.take() {
                target.max_output_tokens = Some(asserted);
            }
        }
        Ok(())
    }

    /// Project one provider generation into an attributed catalog.
    ///
    /// `snapshot` is borrowed and never modified. Every row keeps the
    /// provider's descriptor verbatim; assertions are attributed beside it,
    /// carrying both what the user claimed and what the provider published.
    /// An override naming a model this generation does not publish is reported
    /// through [`AttributedCatalog::unmatched`] instead of adding a row: an
    /// override attributes catalog rows, it never conjures them.
    #[must_use]
    pub fn attribute(&self, snapshot: &CatalogSnapshot) -> AttributedCatalog {
        let revision = snapshot.revision;
        let fetched_at_ms = snapshot.fetched_at_ms;
        let provider_id = snapshot.provider.id.as_str();

        let provider_entries = self.entries.get(provider_id);
        let models = snapshot
            .models
            .iter()
            .map(|model| {
                let entry = provider_entries.and_then(|entries| entries.get(model.id.as_str()));
                let capabilities = ModelCapabilityKind::ALL.map(|kind| {
                    let evidence = kind.of(&model.capabilities);
                    match entry.and_then(|entry| entry.capabilities[kind.index()].as_ref()) {
                        Some((asserted, source)) => AttributedSupport::asserted(
                            kind,
                            *asserted,
                            evidence,
                            revision,
                            fetched_at_ms,
                            source.clone(),
                        ),
                        None => AttributedSupport::evidenced(evidence, revision, fetched_at_ms),
                    }
                });
                let context_window = attribute_limit(
                    ModelLimitField::ContextWindow,
                    model.context_window,
                    entry.and_then(|entry| entry.context_window.as_ref()),
                    revision,
                    fetched_at_ms,
                );
                let max_output_tokens = attribute_limit(
                    ModelLimitField::MaxOutputTokens,
                    model.max_output_tokens,
                    entry.and_then(|entry| entry.max_output_tokens.as_ref()),
                    revision,
                    fetched_at_ms,
                );
                AttributedModel::new(
                    model.clone(),
                    capabilities,
                    context_window,
                    max_output_tokens,
                )
            })
            .collect();

        let unmatched = provider_entries
            .into_iter()
            .flatten()
            .filter(|(model, _)| !snapshot.models.iter().any(|row| &row.id == *model))
            .flat_map(|(model, entry)| {
                entry
                    .sources()
                    .into_iter()
                    .map(move |source| UnmatchedOverride::new(provider_id, model, source.clone()))
            })
            .collect();

        AttributedCatalog::new(
            snapshot.provider.clone(),
            models,
            revision,
            fetched_at_ms,
            self.captured_at_ms,
            unmatched,
        )
    }
}

fn attribute_limit(
    field: ModelLimitField,
    evidence: Option<u64>,
    asserted: Option<&(NonZeroU64, OverrideSource)>,
    revision: u64,
    fetched_at_ms: u64,
) -> AttributedLimit {
    match asserted {
        Some((value, source)) => AttributedLimit::asserted(
            field,
            *value,
            evidence,
            revision,
            fetched_at_ms,
            source.clone(),
        ),
        None => AttributedLimit::evidenced(evidence, revision, fetched_at_ms),
    }
}

fn parse_support(value: &str) -> Option<CapabilitySupport> {
    match value {
        "supported" => Some(CapabilitySupport::Supported),
        "unsupported" => Some(CapabilitySupport::Unsupported),
        "unknown" => Some(CapabilitySupport::Unknown),
        _ => None,
    }
}

fn read_layer(path: &Path) -> Result<Option<String>, CatalogOverrideError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CatalogOverrideError::io(path, source)),
    };
    if metadata.file_type().is_symlink() {
        return Err(CatalogOverrideError::SymbolicLink {
            path: path.display().to_string(),
        });
    }
    if !metadata.is_file() {
        return Err(CatalogOverrideError::WrongFileType {
            path: path.display().to_string(),
            expected: "a regular file",
        });
    }
    if metadata.len() > MAX_OVERRIDE_BYTES {
        return Err(CatalogOverrideError::TooLarge {
            path: path.display().to_string(),
            max_bytes: MAX_OVERRIDE_BYTES,
        });
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|source| CatalogOverrideError::io(path, source))
}
