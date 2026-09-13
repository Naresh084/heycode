//! Hidden ATT04 audio request/output groundwork.

use std::collections::BTreeSet;

use sha2::{Digest as _, Sha256};

use crate::{
    AuthenticationBinding, CapabilitySupport, ChatRequest, FinishReason, InferenceTarget, LlmError,
    ModelDescriptor, ProviderProtocol, Role, TokenUsage,
};

const MAX_AUDIO_BYTES: usize = 32 * 1024 * 1024;
const MAX_AUDIO_ITEMS: u8 = 16;

/// Product visibility of an experimental capability descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentalFeatureVisibility {
    /// Not included in ordinary catalogs, pickers, commands or provider claims.
    Hidden,
}

/// Exact model/format evidence for the hidden ATT04 adapter boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct ExperimentalAudioDescriptor {
    provider: String,
    model: String,
    protocol: ProviderProtocol,
    target: InferenceTarget,
    authentication: AuthenticationBinding,
    input_support: CapabilitySupport,
    output_support: CapabilitySupport,
    input_media_types: Vec<heycode_core::AttachmentMediaType>,
    output_media_types: Vec<heycode_core::AttachmentMediaType>,
    max_inputs: u8,
    max_input_bytes: usize,
}

impl ExperimentalAudioDescriptor {
    /// Construct an exact hidden route before direction-specific evidence.
    ///
    /// # Errors
    /// Blank identity, unknown protocol or unsafe target metadata fails.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        protocol: ProviderProtocol,
        target: InferenceTarget,
        authentication: AuthenticationBinding,
    ) -> Result<Self, ExperimentalAudioError> {
        let descriptor = Self {
            provider: provider.into(),
            model: model.into(),
            protocol,
            target,
            authentication,
            input_support: CapabilitySupport::Unknown,
            output_support: CapabilitySupport::Unknown,
            input_media_types: Vec::new(),
            output_media_types: Vec::new(),
            max_inputs: 0,
            max_input_bytes: 0,
        };
        descriptor.validate_identity()?;
        Ok(descriptor)
    }

    /// Attach exact input evidence and projection bounds.
    ///
    /// The format list describes the adapter's exact projection vocabulary;
    /// model support remains independently tri-state.
    ///
    /// # Errors
    /// Empty/duplicate/non-audio formats or invalid count/byte bounds fail.
    pub fn with_input<I, S>(
        mut self,
        support: CapabilitySupport,
        media_types: I,
        max_inputs: u8,
        max_input_bytes: usize,
    ) -> Result<Self, ExperimentalAudioError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.input_media_types = audio_media_types(media_types)?;
        if !(1..=MAX_AUDIO_ITEMS).contains(&max_inputs)
            || !(1..=MAX_AUDIO_BYTES).contains(&max_input_bytes)
        {
            return Err(ExperimentalAudioError::InvalidDescriptor);
        }
        self.input_support = support;
        self.max_inputs = max_inputs;
        self.max_input_bytes = max_input_bytes;
        Ok(self)
    }

    /// Attach exact output evidence and accepted output formats.
    ///
    /// # Errors
    /// Empty/duplicate/non-audio formats fail.
    pub fn with_output<I, S>(
        mut self,
        support: CapabilitySupport,
        media_types: I,
    ) -> Result<Self, ExperimentalAudioError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.output_media_types = audio_media_types(media_types)?;
        self.output_support = support;
        Ok(self)
    }

    /// This descriptor never enters ordinary product discovery.
    #[must_use]
    pub const fn visibility(&self) -> ExperimentalFeatureVisibility {
        ExperimentalFeatureVisibility::Hidden
    }

    /// Exact provider owner.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Exact canonical model.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Exact existing provider protocol family.
    #[must_use]
    pub const fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    /// Secret-free target.
    #[must_use]
    pub const fn target(&self) -> &InferenceTarget {
        &self.target
    }

    /// Secret-free authentication binding.
    #[must_use]
    pub const fn authentication(&self) -> &AuthenticationBinding {
        &self.authentication
    }

    /// Exact input support evidence.
    #[must_use]
    pub const fn input_support(&self) -> CapabilitySupport {
        self.input_support
    }

    /// Exact output support evidence.
    #[must_use]
    pub const fn output_support(&self) -> CapabilitySupport {
        self.output_support
    }

    /// Whether the descriptor can project this exact input MIME.
    #[must_use]
    pub fn accepts_input(&self, media_type: &heycode_core::AttachmentMediaType) -> bool {
        self.input_media_types.contains(media_type)
    }

    /// Validate one pending output against exact evidence and format.
    ///
    /// # Errors
    /// Unsupported, unproven or differently formatted output fails.
    pub fn validate_output(
        &self,
        output: &ExperimentalAudioOutput,
    ) -> Result<(), ExperimentalAudioError> {
        match self.output_support {
            CapabilitySupport::Supported => {}
            CapabilitySupport::Unsupported => {
                return Err(ExperimentalAudioError::UnsupportedOutput);
            }
            CapabilitySupport::Unknown => return Err(ExperimentalAudioError::UnprovenOutput),
        }
        if !self.output_media_types.contains(output.media_type()) {
            return Err(ExperimentalAudioError::UnsupportedFormat);
        }
        Ok(())
    }

    fn validate_identity(&self) -> Result<(), ExperimentalAudioError> {
        if invalid_id(&self.provider)
            || invalid_id(&self.model)
            || self.protocol == ProviderProtocol::Unknown
        {
            return Err(ExperimentalAudioError::InvalidDescriptor);
        }
        match &self.target {
            InferenceTarget::Http { base_url } => {
                let authority = base_url
                    .strip_prefix("http://")
                    .or_else(|| base_url.strip_prefix("https://"))
                    .and_then(|remainder| remainder.split(['/', '?', '#']).next());
                if base_url.chars().any(char::is_whitespace)
                    || authority.is_none_or(|value| value.is_empty() || value.contains('@'))
                {
                    return Err(ExperimentalAudioError::InvalidDescriptor);
                }
            }
            InferenceTarget::ManagedService { service, location } => {
                if invalid_id(service) || location.as_deref().is_some_and(invalid_id) {
                    return Err(ExperimentalAudioError::InvalidDescriptor);
                }
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for ExperimentalAudioDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExperimentalAudioDescriptor")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("protocol", &self.protocol)
            .field("visibility", &self.visibility())
            .field("input_support", &self.input_support)
            .field("output_support", &self.output_support)
            .field("input_formats", &self.input_media_types)
            .field("output_formats", &self.output_media_types)
            .finish_non_exhaustive()
    }
}

/// One exact admitted audio object attached to a user-message position.
#[derive(Clone, PartialEq, Eq)]
pub struct ExperimentalAudioInput {
    message_index: u32,
    attachment: heycode_core::AttachmentMetadata,
    bytes: Vec<u8>,
}

impl ExperimentalAudioInput {
    /// Bind verified bytes to their durable metadata and message position.
    ///
    /// # Errors
    /// Non-audio metadata, length/hash mismatch, empty/oversized bytes or an
    /// out-of-range message index fails.
    pub fn new(
        message_index: u32,
        attachment: heycode_core::AttachmentMetadata,
        bytes: Vec<u8>,
    ) -> Result<Self, ExperimentalAudioError> {
        attachment
            .validate()
            .map_err(|_| ExperimentalAudioError::InvalidInput)?;
        if !attachment.media_type().is_audio()
            || attachment.audio().is_none()
            || bytes.is_empty()
            || bytes.len() > MAX_AUDIO_BYTES
            || u64::try_from(bytes.len()).ok() != Some(attachment.byte_len())
            || heycode_core::AttachmentContentId::from_sha256(Sha256::digest(&bytes).into())
                != *attachment.content_id()
        {
            return Err(ExperimentalAudioError::InvalidInput);
        }
        Ok(Self {
            message_index,
            attachment,
            bytes,
        })
    }

    /// Zero-based message index in the base request.
    #[must_use]
    pub const fn message_index(&self) -> u32 {
        self.message_index
    }

    /// Exact durable association.
    #[must_use]
    pub const fn attachment(&self) -> &heycode_core::AttachmentMetadata {
        &self.attachment
    }

    /// Exact verified bytes for adapter projection.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for ExperimentalAudioInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExperimentalAudioInput")
            .field("message_index", &self.message_index)
            .field("attachment", &self.attachment)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

/// One hidden experimental request: ordinary neutral text plus exact audio.
#[derive(Clone)]
pub struct ExperimentalAudioRequest {
    base: ChatRequest,
    inputs: Vec<ExperimentalAudioInput>,
}

impl ExperimentalAudioRequest {
    /// Construct a request whose audio positions name user messages.
    ///
    /// # Errors
    /// Empty audio, duplicate content or invalid/non-user message positions
    /// fail.
    pub fn new(
        base: ChatRequest,
        inputs: Vec<ExperimentalAudioInput>,
    ) -> Result<Self, ExperimentalAudioError> {
        if inputs.is_empty() || inputs.len() > usize::from(MAX_AUDIO_ITEMS) {
            return Err(ExperimentalAudioError::InvalidInput);
        }
        let mut ids = BTreeSet::new();
        for input in &inputs {
            let index = usize::try_from(input.message_index)
                .map_err(|_| ExperimentalAudioError::InvalidInput)?;
            if base.messages.get(index).map(|message| message.role) != Some(Role::User)
                || !ids.insert(input.attachment.content_id().as_str())
            {
                return Err(ExperimentalAudioError::InvalidInput);
            }
        }
        Ok(Self { base, inputs })
    }

    /// Ordinary text/tool request plane.
    #[must_use]
    pub const fn base(&self) -> &ChatRequest {
        &self.base
    }

    /// Ordered exact audio inputs.
    #[must_use]
    pub fn inputs(&self) -> &[ExperimentalAudioInput] {
        &self.inputs
    }
}

impl std::fmt::Debug for ExperimentalAudioRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExperimentalAudioRequest")
            .field("model", &self.base.model)
            .field("message_count", &self.base.messages.len())
            .field("audio_count", &self.inputs.len())
            .finish()
    }
}

/// Pending provider audio output. Encoded bytes are never serializable.
#[derive(Clone, PartialEq, Eq)]
pub struct ExperimentalAudioOutput {
    media_type: heycode_core::AttachmentMediaType,
    audio: heycode_core::AttachmentAudioMetadata,
    bytes: Vec<u8>,
}

impl ExperimentalAudioOutput {
    /// Construct one bounded pending output.
    ///
    /// # Errors
    /// Non-audio MIME or empty/>32 MiB bytes fail.
    pub fn new(
        media_type: heycode_core::AttachmentMediaType,
        audio: heycode_core::AttachmentAudioMetadata,
        bytes: Vec<u8>,
    ) -> Result<Self, ExperimentalAudioError> {
        if !media_type.is_audio() || bytes.is_empty() || bytes.len() > MAX_AUDIO_BYTES {
            return Err(ExperimentalAudioError::InvalidOutput);
        }
        Ok(Self {
            media_type,
            audio,
            bytes,
        })
    }

    /// Claimed canonical output MIME.
    #[must_use]
    pub const fn media_type(&self) -> &heycode_core::AttachmentMediaType {
        &self.media_type
    }

    /// Provider-reported stream metadata, independently checked on admission.
    #[must_use]
    pub const fn audio(&self) -> heycode_core::AttachmentAudioMetadata {
        self.audio
    }

    /// Encoded bytes for ATT01 admission.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for ExperimentalAudioOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExperimentalAudioOutput")
            .field("media_type", &self.media_type)
            .field("audio", &self.audio)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

/// Normalized hidden audio stream item.
#[derive(Debug, Clone, PartialEq)]
pub enum ExperimentalAudioEvent {
    /// Visible assistant text delta.
    TextDelta(String),
    /// Complete pending audio body, committed only at terminal success.
    AudioOutput(ExperimentalAudioOutput),
    /// Token usage immediately before finish when present.
    Usage(TokenUsage),
    /// Terminal result.
    Finish(FinishReason),
}

/// Hidden experimental audio stream.
pub type ExperimentalAudioStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<ExperimentalAudioEvent, LlmError>> + Send>>;

/// One exact resolved hidden audio call.
pub struct ResolvedExperimentalAudioCall {
    descriptor: ExperimentalAudioDescriptor,
    request: ExperimentalAudioRequest,
    context_window: Option<u64>,
    model_max_output_tokens: Option<u64>,
    catalog_revision: Option<u64>,
    catalog_fetched_at_ms: Option<u64>,
    effective_at_ms: u64,
}

impl ResolvedExperimentalAudioCall {
    /// Provider owner.
    #[must_use]
    pub fn provider(&self) -> &str {
        self.descriptor.provider()
    }

    /// Exact canonical model.
    #[must_use]
    pub fn model(&self) -> &str {
        self.descriptor.model()
    }

    /// Exact protocol family.
    #[must_use]
    pub const fn protocol(&self) -> ProviderProtocol {
        self.descriptor.protocol()
    }

    /// Target.
    #[must_use]
    pub const fn target(&self) -> &InferenceTarget {
        self.descriptor.target()
    }

    /// Authentication binding.
    #[must_use]
    pub const fn authentication(&self) -> &AuthenticationBinding {
        self.descriptor.authentication()
    }

    /// Ordinary request plane.
    #[must_use]
    pub const fn base(&self) -> &ChatRequest {
        self.request.base()
    }

    /// Exact audio inputs.
    #[must_use]
    pub fn inputs(&self) -> &[ExperimentalAudioInput] {
        self.request.inputs()
    }

    /// Context capacity.
    #[must_use]
    pub const fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    /// Model output ceiling.
    #[must_use]
    pub const fn model_max_output_tokens(&self) -> Option<u64> {
        self.model_max_output_tokens
    }

    /// Catalog revision.
    #[must_use]
    pub const fn catalog_revision(&self) -> Option<u64> {
        self.catalog_revision
    }

    /// Catalog commit time.
    #[must_use]
    pub const fn catalog_fetched_at_ms(&self) -> Option<u64> {
        self.catalog_fetched_at_ms
    }

    /// Capability comparison instant.
    #[must_use]
    pub const fn effective_at_ms(&self) -> u64 {
        self.effective_at_ms
    }
}

impl std::fmt::Debug for ResolvedExperimentalAudioCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedExperimentalAudioCall")
            .field("descriptor", &self.descriptor)
            .field("message_count", &self.request.base.messages.len())
            .field("audio_count", &self.request.inputs.len())
            .finish_non_exhaustive()
    }
}

/// Resolve one hidden request against exact descriptor/model evidence.
///
/// # Errors
/// Route/model mismatch, unsupported/unproven input, format/bounds or invalid
/// catalog facts fail before the adapter stream exists.
pub fn resolve_experimental_audio(
    descriptor: &ExperimentalAudioDescriptor,
    request: ExperimentalAudioRequest,
    model: &ModelDescriptor,
    catalog_revision: Option<u64>,
    catalog_fetched_at_ms: Option<u64>,
    effective_at_ms: u64,
) -> Result<ResolvedExperimentalAudioCall, ExperimentalAudioError> {
    if request.base.model != descriptor.model || model.id != descriptor.model {
        return Err(ExperimentalAudioError::ModelMismatch {
            requested: request.base.model,
            resolved: descriptor.model.clone(),
        });
    }
    match descriptor.input_support {
        CapabilitySupport::Supported => {}
        CapabilitySupport::Unsupported => return Err(ExperimentalAudioError::UnsupportedInput),
        CapabilitySupport::Unknown => return Err(ExperimentalAudioError::UnprovenInput),
    }
    let total = request.inputs.iter().try_fold(0_usize, |total, input| {
        if !descriptor.accepts_input(input.attachment.media_type()) {
            return Err(ExperimentalAudioError::UnsupportedFormat);
        }
        total
            .checked_add(input.bytes.len())
            .ok_or(ExperimentalAudioError::InvalidInput)
    })?;
    if request.inputs.len() > usize::from(descriptor.max_inputs)
        || total > descriptor.max_input_bytes
        || effective_at_ms == 0
        || catalog_revision.is_some() != catalog_fetched_at_ms.is_some()
        || catalog_revision == Some(0)
        || catalog_fetched_at_ms == Some(0)
    {
        return Err(ExperimentalAudioError::InvalidInput);
    }
    Ok(ResolvedExperimentalAudioCall {
        descriptor: descriptor.clone(),
        request,
        context_window: model.context_window,
        model_max_output_tokens: model.max_output_tokens,
        catalog_revision,
        catalog_fetched_at_ms,
        effective_at_ms,
    })
}

/// Provider-owned hidden experimental audio operation.
pub trait ExperimentalAudioAdapter: Send + Sync {
    /// Exact model/format/capability evidence.
    fn descriptor(&self) -> &ExperimentalAudioDescriptor;

    /// Consume one resolved call and begin streaming.
    fn stream(&self, call: ResolvedExperimentalAudioCall) -> ExperimentalAudioStream;

    /// Consume one resolved call under a caller-owned operation token.
    fn stream_cancellable(
        &self,
        call: ResolvedExperimentalAudioCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ExperimentalAudioStream {
        if cancellation.is_cancelled() {
            Box::pin(futures::stream::once(async {
                Err(crate::retry::cancelled_error())
            }))
        } else {
            self.stream(call)
        }
    }
}

/// Stable hidden-audio validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExperimentalAudioError {
    /// Descriptor identity/target/format bounds are invalid.
    #[error("experimental audio descriptor is invalid")]
    InvalidDescriptor,
    /// Request/model does not match the descriptor's exact model.
    #[error("experimental audio model `{requested}` does not match `{resolved}`")]
    ModelMismatch {
        /// Request/model id.
        requested: String,
        /// Descriptor id.
        resolved: String,
    },
    /// Model evidence explicitly denies audio input.
    #[error("experimental audio input is unsupported")]
    UnsupportedInput,
    /// Model audio-input evidence is unknown.
    #[error("experimental audio input support is unproven")]
    UnprovenInput,
    /// Model evidence explicitly denies audio output.
    #[error("experimental audio output is unsupported")]
    UnsupportedOutput,
    /// Model audio-output evidence is unknown.
    #[error("experimental audio output support is unproven")]
    UnprovenOutput,
    /// MIME is outside the descriptor's exact projection vocabulary.
    #[error("experimental audio format is unsupported")]
    UnsupportedFormat,
    /// Input bytes/metadata/message association or request bounds are invalid.
    #[error("experimental audio input is invalid")]
    InvalidInput,
    /// Output bytes/metadata are invalid.
    #[error("experimental audio output is invalid")]
    InvalidOutput,
}

fn audio_media_types<I, S>(
    values: I,
) -> Result<Vec<heycode_core::AttachmentMediaType>, ExperimentalAudioError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut output = Vec::new();
    for value in values {
        let media_type = heycode_core::AttachmentMediaType::new(value.as_ref())
            .map_err(|_| ExperimentalAudioError::InvalidDescriptor)?;
        if !media_type.is_audio() || output.contains(&media_type) {
            return Err(ExperimentalAudioError::InvalidDescriptor);
        }
        output.push(media_type);
    }
    if output.is_empty() {
        return Err(ExperimentalAudioError::InvalidDescriptor);
    }
    Ok(output)
}

fn invalid_id(value: &str) -> bool {
    value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || value.chars().any(char::is_control)
}
