//! Neutral durable facts from one successful provider response.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

const PROVIDER_RESPONSE_METADATA_SCHEMA_VERSION: u32 = 1;
const MAX_CONTEXT_EDITS: usize = 16;

/// Safe validation failure for detailed response facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderResponseMetadataError {
    /// Schema version is unsupported.
    #[error("provider response metadata schema is unsupported")]
    UnsupportedSchema,
    /// Cache counters are inconsistent or overflowed.
    #[error("provider cache usage is invalid")]
    InvalidCacheUsage,
    /// Context-edit facts are empty, duplicated or inconsistent.
    #[error("provider context-edit metadata is invalid")]
    InvalidContextEdits,
    /// The metadata object carries no evidence.
    #[error("provider response metadata is empty")]
    Empty,
}

/// Whether detailed counters prove a cache read and/or write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCacheActivity {
    /// Neither cache counter is non-zero.
    None,
    /// Existing cached input was read.
    Read,
    /// New cached input was written.
    Write,
    /// This response both read and wrote cached input.
    ReadAndWrite,
}

/// Exact neutral cache counters for one successful response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCacheUsage {
    schema_version: u32,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    cache_write_tokens_unknown: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uncached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_write_5m_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_write_1h_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_tokens: Option<u64>,
}

impl ProviderCacheUsage {
    /// Build normalized totals and cache read/write counters.
    ///
    /// # Errors
    /// A cache component larger than total input is inconsistent.
    pub fn new(
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> Result<Self, ProviderResponseMetadataError> {
        let usage = Self {
            schema_version: PROVIDER_RESPONSE_METADATA_SCHEMA_VERSION,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cache_write_tokens_unknown: false,
            uncached_input_tokens: None,
            cache_write_5m_tokens: None,
            cache_write_1h_tokens: None,
            reasoning_tokens: None,
        };
        usage.validate()?;
        Ok(usage)
    }

    /// Record an observed cache-read count when the provider omitted writes.
    /// Missing counters are not evidence of a zero-write response.
    pub fn with_unknown_cache_writes(mut self) -> Self {
        self.cache_write_tokens = 0;
        self.cache_write_tokens_unknown = true;
        self.uncached_input_tokens = None;
        self.cache_write_5m_tokens = None;
        self.cache_write_1h_tokens = None;
        self
    }

    /// Exact cache writes when reported; otherwise unknown.
    #[must_use]
    pub const fn reported_cache_write_tokens(self) -> Option<u64> {
        if self.cache_write_tokens_unknown {
            None
        } else {
            Some(self.cache_write_tokens)
        }
    }

    /// Attach a provider-reported uncached partition.
    ///
    /// # Errors
    /// Uncached + cache-read + cache-write must equal total input.
    pub fn with_uncached_input_tokens(
        mut self,
        tokens: u64,
    ) -> Result<Self, ProviderResponseMetadataError> {
        self.uncached_input_tokens = Some(tokens);
        self.validate()?;
        Ok(self)
    }

    /// Attach five-minute and one-hour cache-write partitions.
    ///
    /// # Errors
    /// Their checked sum must equal the cache-write total.
    pub fn with_cache_write_ttl_tokens(
        mut self,
        five_minutes: u64,
        one_hour: u64,
    ) -> Result<Self, ProviderResponseMetadataError> {
        self.cache_write_5m_tokens = Some(five_minutes);
        self.cache_write_1h_tokens = Some(one_hour);
        self.validate()?;
        Ok(self)
    }

    /// Attach the reasoning subset of generated output.
    ///
    /// # Errors
    /// Reasoning cannot exceed total output.
    pub fn with_reasoning_tokens(
        mut self,
        tokens: u64,
    ) -> Result<Self, ProviderResponseMetadataError> {
        self.reasoning_tokens = Some(tokens);
        self.validate()?;
        Ok(self)
    }

    /// Revalidate deserialized detailed counters.
    ///
    /// # Errors
    /// Unsupported schema, impossible component bounds/partitions or overflow.
    pub fn validate(&self) -> Result<(), ProviderResponseMetadataError> {
        if self.cache_write_tokens_unknown
            && (self.cache_write_tokens != 0
                || self.uncached_input_tokens.is_some()
                || self.cache_write_5m_tokens.is_some()
                || self.cache_write_1h_tokens.is_some())
        {
            return Err(ProviderResponseMetadataError::InvalidCacheUsage);
        }
        if self.schema_version != PROVIDER_RESPONSE_METADATA_SCHEMA_VERSION
            || self.cache_read_tokens > self.input_tokens
            || self.cache_write_tokens > self.input_tokens
            || self
                .reasoning_tokens
                .is_some_and(|tokens| tokens > self.output_tokens)
        {
            return Err(
                if self.schema_version != PROVIDER_RESPONSE_METADATA_SCHEMA_VERSION {
                    ProviderResponseMetadataError::UnsupportedSchema
                } else {
                    ProviderResponseMetadataError::InvalidCacheUsage
                },
            );
        }
        if self.uncached_input_tokens.is_some_and(|uncached| {
            uncached
                .checked_add(self.cache_read_tokens)
                .and_then(|total| total.checked_add(self.cache_write_tokens))
                != Some(self.input_tokens)
        }) {
            return Err(ProviderResponseMetadataError::InvalidCacheUsage);
        }
        match (self.cache_write_5m_tokens, self.cache_write_1h_tokens) {
            (None, None) => {}
            (Some(five), Some(hour)) if five.checked_add(hour) == Some(self.cache_write_tokens) => {
            }
            _ => return Err(ProviderResponseMetadataError::InvalidCacheUsage),
        }
        Ok(())
    }

    /// Normalized total input tokens.
    #[must_use]
    pub const fn input_tokens(self) -> u64 {
        self.input_tokens
    }

    /// Generated output tokens.
    #[must_use]
    pub const fn output_tokens(self) -> u64 {
        self.output_tokens
    }

    /// Input tokens read from cache.
    #[must_use]
    pub const fn cache_read_tokens(self) -> u64 {
        self.cache_read_tokens
    }

    /// Input tokens written to cache.
    #[must_use]
    pub const fn cache_write_tokens(self) -> u64 {
        self.cache_write_tokens
    }

    /// Provider-reported uncached input partition.
    #[must_use]
    pub const fn uncached_input_tokens(self) -> Option<u64> {
        self.uncached_input_tokens
    }

    /// Five-minute cache-write partition.
    #[must_use]
    pub const fn cache_write_5m_tokens(self) -> Option<u64> {
        self.cache_write_5m_tokens
    }

    /// One-hour cache-write partition.
    #[must_use]
    pub const fn cache_write_1h_tokens(self) -> Option<u64> {
        self.cache_write_1h_tokens
    }

    /// Reasoning subset of output.
    #[must_use]
    pub const fn reasoning_tokens(self) -> Option<u64> {
        self.reasoning_tokens
    }

    /// Visible read/write activity.
    #[must_use]
    pub const fn activity(self) -> ProviderCacheActivity {
        match (self.cache_read_tokens > 0, self.cache_write_tokens > 0) {
            (false, false) => ProviderCacheActivity::None,
            (true, false) => ProviderCacheActivity::Read,
            (false, true) => ProviderCacheActivity::Write,
            (true, true) => ProviderCacheActivity::ReadAndWrite,
        }
    }
}

/// Provider-neutral context-edit family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEditKind {
    /// Thinking turns were cleared.
    ClearThinking,
    /// Tool-use/result pairs were cleared.
    ClearToolUses,
}

/// One applied context edit with exact safe counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderContextEdit {
    kind: ContextEditKind,
    cleared_units: u64,
    cleared_input_tokens: u64,
}

impl ProviderContextEdit {
    /// Build one applied edit.
    ///
    /// # Errors
    /// Applied edits must clear a positive unit and token count.
    pub fn new(
        kind: ContextEditKind,
        cleared_units: u64,
        cleared_input_tokens: u64,
    ) -> Result<Self, ProviderResponseMetadataError> {
        if cleared_units == 0 || cleared_input_tokens == 0 {
            return Err(ProviderResponseMetadataError::InvalidContextEdits);
        }
        Ok(Self {
            kind,
            cleared_units,
            cleared_input_tokens,
        })
    }

    /// Applied edit family.
    #[must_use]
    pub const fn kind(&self) -> ContextEditKind {
        self.kind
    }

    /// Cleared provider units (thinking turns or tool uses).
    #[must_use]
    pub const fn cleared_units(&self) -> u64 {
        self.cleared_units
    }

    /// Provider-reported cleared input tokens.
    #[must_use]
    pub const fn cleared_input_tokens(&self) -> u64 {
        self.cleared_input_tokens
    }
}

/// Observed cache consequence of context editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePrefixImpact {
    /// No edit applied; the prefix stayed reusable.
    Preserved,
    /// At least one edit invalidated the prefix at its edit point.
    InvalidatedAtEdit,
}

/// Complete neutral detailed facts for one successful provider response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderResponseMetadata {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_usage: Option<ProviderCacheUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    context_edits: Vec<ProviderContextEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_prefix_impact: Option<CachePrefixImpact>,
}

impl ProviderResponseMetadata {
    /// Build one detailed response fact set.
    ///
    /// # Errors
    /// Empty, duplicate, overflowed or cache-impact-inconsistent facts fail.
    pub fn new(
        cache_usage: Option<ProviderCacheUsage>,
        context_edits: Vec<ProviderContextEdit>,
        cache_prefix_impact: Option<CachePrefixImpact>,
    ) -> Result<Self, ProviderResponseMetadataError> {
        let metadata = Self {
            schema_version: PROVIDER_RESPONSE_METADATA_SCHEMA_VERSION,
            cache_usage,
            context_edits,
            cache_prefix_impact,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Revalidate a deserialized fact set.
    ///
    /// # Errors
    /// Unsupported schema, invalid cache counters, duplicated/overflowed edits
    /// or a cache impact contradicting the edit list.
    pub fn validate(&self) -> Result<(), ProviderResponseMetadataError> {
        if self.schema_version != PROVIDER_RESPONSE_METADATA_SCHEMA_VERSION {
            return Err(ProviderResponseMetadataError::UnsupportedSchema);
        }
        if let Some(cache) = self.cache_usage {
            cache.validate()?;
        }
        if self.cache_usage.is_none()
            && self.context_edits.is_empty()
            && self.cache_prefix_impact.is_none()
        {
            return Err(ProviderResponseMetadataError::Empty);
        }
        if self.context_edits.len() > MAX_CONTEXT_EDITS {
            return Err(ProviderResponseMetadataError::InvalidContextEdits);
        }
        let mut kinds = BTreeSet::new();
        let mut total = 0_u64;
        for edit in &self.context_edits {
            if edit.cleared_units == 0 || edit.cleared_input_tokens == 0 || !kinds.insert(edit.kind)
            {
                return Err(ProviderResponseMetadataError::InvalidContextEdits);
            }
            total = total
                .checked_add(edit.cleared_input_tokens)
                .ok_or(ProviderResponseMetadataError::InvalidContextEdits)?;
        }
        match (self.context_edits.is_empty(), self.cache_prefix_impact) {
            (true, Some(CachePrefixImpact::InvalidatedAtEdit))
            | (false, Some(CachePrefixImpact::Preserved))
            | (false, None) => Err(ProviderResponseMetadataError::InvalidContextEdits),
            _ => Ok(()),
        }
    }

    /// Detailed cache counters, when the provider reported them.
    #[must_use]
    pub const fn cache_usage(&self) -> Option<ProviderCacheUsage> {
        self.cache_usage
    }

    /// Applied context edits in provider order.
    #[must_use]
    pub fn context_edits(&self) -> &[ProviderContextEdit] {
        &self.context_edits
    }

    /// Observed cache-prefix consequence.
    #[must_use]
    pub const fn cache_prefix_impact(&self) -> Option<CachePrefixImpact> {
        self.cache_prefix_impact
    }

    /// Checked total provider-reported tokens cleared by edits.
    #[must_use]
    pub fn total_cleared_input_tokens(&self) -> Option<u64> {
        self.context_edits
            .iter()
            .map(ProviderContextEdit::cleared_input_tokens)
            .try_fold(0_u64, u64::checked_add)
    }

    /// Whether the provider observed an edit-point cache invalidation.
    #[must_use]
    pub const fn invalidated_cache_prefix(&self) -> bool {
        matches!(
            self.cache_prefix_impact,
            Some(CachePrefixImpact::InvalidatedAtEdit)
        )
    }
}
