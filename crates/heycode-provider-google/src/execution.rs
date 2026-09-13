//! PGCP06 provider-owned Google code-execution request and event projection.
//!
//! The Gemini GenerateContent discovery schema exposes an empty
//! `Tool.codeExecution` request object and two response-only `Part` members:
//! `executableCode` and `codeExecutionResult`. The generated code is Python,
//! and the result reports `OK`, `FAILED`, or `DEADLINE_EXCEEDED`.
//!
//! This module keeps two planes separate. Shared-adapter integration must keep
//! the complete parts in `GeminiModelContent` provider state for exact replay;
//! the projector emits bounded provider-neutral call/result events for durable
//! inspection. Result stdout/stderr never enters the neutral result, whose
//! vocabulary intentionally retains only outcome metadata.
//!
//! Sources:
//! - <https://generativelanguage.googleapis.com/$discovery/rest?version=v1beta>
//! - <https://ai.google.dev/gemini-api/docs/code-execution>

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use heycode_core::{CallId, ProviderRequestOption, ServerToolCall, ServerToolResult};
use heycode_llm::InferenceEvent;

/// Logical native-tool capability implemented by Gemini code execution.
pub const GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL: &str = "code_execution";

/// Provider-owned implementation id registered with the native-tool router.
pub const GOOGLE_CODE_EXECUTION_IMPLEMENTATION: &str = "google:code_execution";

/// Provider request-option kind carrying the `codeExecution` tool entry.
pub const GOOGLE_CODE_EXECUTION_OPTION_KIND: &str = "google-code-execution";

/// Provider-native tool name retained on normalized call events.
pub const GOOGLE_CODE_EXECUTION_TOOL_NAME: &str = "code_execution";

const TOOL_FIELD: &str = "codeExecution";
const MAX_NORMALIZED_CODE_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_ID_BYTES: usize = 128;

/// One request to make Gemini's server-side Python executor available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodeExecutionRequest;

impl CodeExecutionRequest {
    /// Construct a code-execution request.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Exact `tools[]` entry for GenerateContent.
    #[must_use]
    pub fn tool_entry(self) -> serde_json::Value {
        serde_json::json!({ TOOL_FIELD: {} })
    }

    /// Provider-owned request option carrying the tool entry.
    ///
    /// # Errors
    /// Fails if the fixed entry ever stops satisfying the shared option
    /// identity or size contract.
    pub fn provider_option(self) -> Result<ProviderRequestOption, CodeExecutionError> {
        ProviderRequestOption::new(
            crate::catalog::GOOGLE_PROVIDER,
            GOOGLE_CODE_EXECUTION_OPTION_KIND,
            serde_json::json!({ "tool": self.tool_entry() }),
        )
        .map_err(|_| CodeExecutionError::InvalidRequest)
    }
}

/// Accumulates code parts and emits correlated neutral call/result events.
pub struct CodeExecutionProjector {
    requested: bool,
    response_id: String,
    next_ordinal: u32,
    seen_provider_ids: BTreeSet<CodePartId>,
    named: BTreeMap<CodePartId, CallId>,
    unnamed: VecDeque<CallId>,
}

impl std::fmt::Debug for CodeExecutionProjector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodeExecutionProjector")
            .field("requested", &self.requested)
            .field("seen_provider_ids", &self.seen_provider_ids.len())
            .field("named_pending", &self.named.len())
            .field("unnamed_pending", &self.unnamed.len())
            .field("next_ordinal", &self.next_ordinal)
            .finish()
    }
}

impl CodeExecutionProjector {
    /// Build one projector for a response.
    ///
    /// # Errors
    /// A response identity that cannot form a bounded call id fails before any
    /// provider part is observed.
    pub fn new(
        request: Option<CodeExecutionRequest>,
        response_id: impl Into<String>,
    ) -> Result<Self, CodeExecutionError> {
        let response_id = response_id.into();
        let probe = CallId::from_raw(format!("{response_id}/code/0"));
        ServerToolCall::new(
            probe,
            GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
            GOOGLE_CODE_EXECUTION_TOOL_NAME,
            serde_json::json!({}),
        )
        .map_err(|_| CodeExecutionError::InvalidCallIdentity)?;
        Ok(Self {
            requested: request.is_some(),
            response_id,
            next_ordinal: 0,
            seen_provider_ids: BTreeSet::new(),
            named: BTreeMap::new(),
            unnamed: VecDeque::new(),
        })
    }

    /// Observe one exact Gemini `Part` and return any normalized event it owns.
    ///
    /// Parts without code execution are ignored. A part may contain at most
    /// one of `executableCode` and `codeExecutionResult`, matching the Part
    /// content union enforced by the shared adapter.
    ///
    /// # Errors
    /// Unsolicited, malformed, duplicate, uncorrelated, or unsupported parts
    /// fail rather than producing approximate durable history.
    pub fn observe_part(
        &mut self,
        output_index: u32,
        part: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, CodeExecutionError> {
        let part = part.as_object().ok_or(CodeExecutionError::Malformed(
            "code-execution part must be an object",
        ))?;
        let code = part.get("executableCode").filter(|value| !value.is_null());
        let result = part
            .get("codeExecutionResult")
            .filter(|value| !value.is_null());
        if code.is_some() && result.is_some() {
            return Err(CodeExecutionError::Malformed(
                "code-execution part set two content fields",
            ));
        }
        if code.is_none() && result.is_none() {
            return Ok(Vec::new());
        }
        if !self.requested {
            return Err(CodeExecutionError::Unsolicited);
        }
        match (code, result) {
            (Some(code), None) => self.executable_code(output_index, code),
            (None, Some(result)) => self.execution_result(output_index, result),
            (None, None) | (Some(_), Some(_)) => Ok(Vec::new()),
        }
    }

    /// Require every generated code part to have a corresponding result.
    ///
    /// # Errors
    /// Any named or unnamed call still open at response settlement fails.
    pub fn finish(&self) -> Result<(), CodeExecutionError> {
        if self.named.is_empty() && self.unnamed.is_empty() {
            Ok(())
        } else {
            Err(CodeExecutionError::UnsettledCall)
        }
    }

    fn executable_code(
        &mut self,
        output_index: u32,
        value: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, CodeExecutionError> {
        let value = value.as_object().ok_or(CodeExecutionError::Malformed(
            "executableCode must be an object",
        ))?;
        let language = value
            .get("language")
            .and_then(serde_json::Value::as_str)
            .ok_or(CodeExecutionError::Malformed(
                "executableCode language must be a string",
            ))?;
        if language != "PYTHON" {
            return Err(CodeExecutionError::UnsupportedLanguage);
        }
        let code = value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .ok_or(CodeExecutionError::Malformed(
                "executableCode code must be a string",
            ))?;
        let provider_id = optional_provider_id(value, "id")?;
        if let Some(provider_id) = &provider_id
            && !self.seen_provider_ids.insert(provider_id.clone())
        {
            return Err(CodeExecutionError::DuplicateId);
        }
        let ordinal = self.next_ordinal;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or(CodeExecutionError::TooManyCalls)?;
        // Provider ids and generated ordinals occupy different identity planes:
        // a provider id `"0"` must not collide with the first unnamed call.
        // The normalized id is always ordinal; the provider id remains in the
        // redacted call input for exact result correlation.
        let call_id = CallId::from_raw(format!("{}/code/{ordinal}", self.response_id));
        let mut input = if code.len() <= MAX_NORMALIZED_CODE_BYTES {
            serde_json::json!({ "language": "python", "code": code })
        } else {
            serde_json::json!({
                "language": "python",
                "code_omitted": true,
                "code_bytes": code.len(),
            })
        };
        if let Some(provider_id) = &provider_id {
            input["provider_id"] = serde_json::json!(provider_id.as_str());
        }
        let call = ServerToolCall::new(
            call_id.clone(),
            GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
            GOOGLE_CODE_EXECUTION_TOOL_NAME,
            input,
        )
        .map_err(|_| CodeExecutionError::InvalidCallIdentity)?;
        if let Some(provider_id) = provider_id {
            self.named.insert(provider_id, call_id);
        } else {
            self.unnamed.push_back(call_id);
        }
        Ok(vec![InferenceEvent::ServerToolCall { output_index, call }])
    }

    fn execution_result(
        &mut self,
        output_index: u32,
        value: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, CodeExecutionError> {
        let value = value.as_object().ok_or(CodeExecutionError::Malformed(
            "codeExecutionResult must be an object",
        ))?;
        let outcome = value
            .get("outcome")
            .and_then(serde_json::Value::as_str)
            .ok_or(CodeExecutionError::Malformed(
                "codeExecutionResult outcome must be a string",
            ))?;
        let outcome = match outcome {
            "OUTCOME_OK" => ExecutionOutcome::Success,
            "OUTCOME_FAILED" => ExecutionOutcome::Failed,
            "OUTCOME_DEADLINE_EXCEEDED" => ExecutionOutcome::DeadlineExceeded,
            "OUTCOME_UNSPECIFIED" => return Err(CodeExecutionError::UnsupportedOutcome),
            _ => return Err(CodeExecutionError::UnsupportedOutcome),
        };
        if let Some(output) = value.get("output").filter(|value| !value.is_null())
            && !output.is_string()
        {
            return Err(CodeExecutionError::Malformed(
                "codeExecutionResult output must be a string",
            ));
        }
        let provider_id = optional_provider_id(value, "id")?;
        let call_id = match provider_id {
            Some(id) => self
                .named
                .remove(&id)
                .ok_or(CodeExecutionError::OrphanResult)?,
            None => self
                .unnamed
                .pop_front()
                .ok_or(CodeExecutionError::OrphanResult)?,
        };
        let result = match outcome {
            ExecutionOutcome::Success => ServerToolResult::success(call_id, None, Vec::new()),
            ExecutionOutcome::Failed => ServerToolResult::error(call_id, "execution_failed"),
            ExecutionOutcome::DeadlineExceeded => {
                ServerToolResult::error(call_id, "deadline_exceeded")
            }
        }
        .map_err(|_| CodeExecutionError::InvalidCallIdentity)?;
        Ok(vec![InferenceEvent::ServerToolResult {
            output_index,
            result,
        }])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionOutcome {
    Success,
    Failed,
    DeadlineExceeded,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CodePartId(String);

impl CodePartId {
    fn new(value: &str) -> Result<Self, CodeExecutionError> {
        // The discovery schema defines this as an opaque string and publishes
        // no character pattern. Bound it and reject controls because it enters
        // durable JSON, but do not invent URL/CallId syntax: this value is used
        // only for exact provider correlation.
        if value.is_empty()
            || value.len() > MAX_PROVIDER_ID_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(CodeExecutionError::InvalidProviderId);
        }
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn optional_provider_id(
    value: &serde_json::Map<String, serde_json::Value>,
    field: &'static str,
) -> Result<Option<CodePartId>, CodeExecutionError> {
    let Some(id) = value.get(field).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let id = id.as_str().ok_or(CodeExecutionError::Malformed(
        "code-execution id must be a string",
    ))?;
    CodePartId::new(id).map(Some)
}

/// Code-execution request or projection failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeExecutionError {
    /// Fixed provider request metadata failed shared validation.
    #[error("Google code-execution request metadata is invalid")]
    InvalidRequest,
    /// Provider response identity cannot form a neutral call identity.
    #[error("Google code-execution call identity is invalid")]
    InvalidCallIdentity,
    /// Code execution appeared on a request that did not enable it.
    #[error("Google code execution was not requested")]
    Unsolicited,
    /// A response shape violated the documented schema.
    #[error("invalid Google code-execution response: {0}")]
    Malformed(&'static str),
    /// The only documented executable language is Python.
    #[error("Google code execution returned an unsupported language")]
    UnsupportedLanguage,
    /// Optional provider part identity is unsafe or unusable.
    #[error("Google code execution returned an invalid part id")]
    InvalidProviderId,
    /// Provider part identity was reused in one response.
    #[error("Google code execution reused a part id")]
    DuplicateId,
    /// Response exceeded the normalized call-identity budget.
    #[error("Google code execution returned too many calls")]
    TooManyCalls,
    /// A result had no matching generated-code part.
    #[error("Google code-execution result has no matching call")]
    OrphanResult,
    /// Provider returned no result for a generated-code part.
    #[error("Google code-execution response ended with an unsettled call")]
    UnsettledCall,
    /// Provider returned an unknown or unspecified outcome.
    #[error("Google code execution returned an unsupported outcome")]
    UnsupportedOutcome,
}

/// Register Gemini code execution as Google's provider-native implementation.
#[must_use]
pub fn google_code_execution_native_tools_plugin() -> Box<dyn heycode_core::Plugin> {
    struct GoogleCodeExecutionNativeToolsPlugin;

    impl heycode_core::Plugin for GoogleCodeExecutionNativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-google-code-execution"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Tool],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::NativeTool,
                GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_native_tools::SERVICE_NATIVE_TOOLS]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let registry = context
                .get::<heycode_native_tools::NativeToolRegistry>(
                    heycode_native_tools::SERVICE_NATIVE_TOOLS,
                )
                .ok_or_else(|| {
                    heycode_core::CoreError::other("native-tools service type mismatch")
                })?;
            let implementation = heycode_native_tools::NativeToolImplementation::new(
                GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
                GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
                heycode_core::NativeToolImplementationKind::Provider,
                Some(crate::catalog::GOOGLE_PROVIDER.to_owned()),
                100,
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            registry
                .register(context, implementation)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }

    Box::new(GoogleCodeExecutionNativeToolsPlugin)
}
