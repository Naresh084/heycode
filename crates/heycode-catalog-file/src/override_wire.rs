//! Explicit schema-v1 wire mapping for a user-authored catalog override
//! document, independent from the runtime attributed types.
//!
//! Two properties of this module are load-bearing:
//!
//! 1. **There is no provenance field.** A document says what a user asserts,
//!    never who asserted it or how well evidenced it is. Provenance is minted
//!    by [`crate::CatalogOverrides`] from the fact that it read these bytes out
//!    of a user override layer, exactly the way
//!    `heycode_core::UntrustedContentBoundary` refuses to let a caller choose the
//!    source of text it did not author. `deny_unknown_fields` on every table
//!    turns an attempt to write one into a loud parse failure rather than a
//!    silently ignored key.
//! 2. **Nothing here implements `Serialize`.** heycode never writes an override
//!    document, so no asserted value can be re-emitted into a file that a later
//!    read could mistake for fetched evidence.

use serde::Deserialize;

/// Version marker read before strict decoding, so a newer document reports its
/// version rather than failing on an unknown field.
#[derive(Deserialize)]
pub(crate) struct WireOverrideProbe {
    #[serde(default)]
    pub(crate) schema_version: Option<u32>,
}

/// One override document.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireOverrideDocument {
    #[allow(dead_code)]
    pub(crate) schema_version: u32,
    /// `[[model]]` entries. Absent means the document asserts nothing.
    #[serde(default, rename = "model")]
    pub(crate) models: Vec<WireModelOverride>,
}

/// One `[[model]]` entry: which catalog row, and what is asserted about it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireModelOverride {
    pub(crate) provider: String,
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) context_window: Option<u64>,
    #[serde(default)]
    pub(crate) max_output_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) capabilities: WireCapabilityOverrides,
}

/// `[model.capabilities]`: each key is an exact tri-state name.
///
/// Values stay `String` rather than a serde enum so an unrecognized spelling
/// reports the exact rejected word instead of a generic variant error.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireCapabilityOverrides {
    #[serde(default)]
    pub(crate) tools: Option<String>,
    #[serde(default)]
    pub(crate) reasoning: Option<String>,
    #[serde(default)]
    pub(crate) image_input: Option<String>,
    #[serde(default)]
    pub(crate) document_input: Option<String>,
    #[serde(default)]
    pub(crate) structured_output: Option<String>,
    #[serde(default)]
    pub(crate) native_web: Option<String>,
    #[serde(default)]
    pub(crate) native_compaction: Option<String>,
    #[serde(default)]
    pub(crate) prompt_cache: Option<String>,
}

impl WireCapabilityOverrides {
    /// Raw value written for one capability kind, in stable kind order.
    pub(crate) fn value(&self, kind: crate::ModelCapabilityKind) -> Option<&str> {
        match kind {
            crate::ModelCapabilityKind::Tools => self.tools.as_deref(),
            crate::ModelCapabilityKind::Reasoning => self.reasoning.as_deref(),
            crate::ModelCapabilityKind::ImageInput => self.image_input.as_deref(),
            crate::ModelCapabilityKind::DocumentInput => self.document_input.as_deref(),
            crate::ModelCapabilityKind::StructuredOutput => self.structured_output.as_deref(),
            crate::ModelCapabilityKind::NativeWeb => self.native_web.as_deref(),
            crate::ModelCapabilityKind::NativeCompaction => self.native_compaction.as_deref(),
            crate::ModelCapabilityKind::PromptCache => self.prompt_cache.as_deref(),
        }
    }
}
