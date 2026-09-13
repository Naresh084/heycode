//! Concrete PL09 adapters for the six product contribution registries.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use async_trait::async_trait;
use futures::Stream;
use heycode_agent::{
    Command, CommandArgument, CommandDescriptor, CommandSource, CommandTiming, SubagentPreset,
    SubagentProviderId,
};
use heycode_core::{Context, PluginContributionSpec, ServiceKey, TokenUsage};
use heycode_extensions::{
    CodePluginCancellationToken, CodePluginContribution, CodePluginInvocationError,
    CodePluginRemoteErrorCode, CodePluginTransportFault, ContributionKind, HostActivationFailure,
    PluginPermission,
};
use heycode_hooks::{Hook, HookAction};
use heycode_llm::{
    ChatMessage, ChatRequest, ChunkStream, FinishReason, LlmError, ModelDescriptor, Provider,
    ProviderDescriptor, ProviderErrorClass, ProviderFailure, ProviderFailureOrigin, ProviderInfo,
    ProviderProtocol, Role, StreamChunk,
};
use heycode_skills::Skill;
use heycode_ui::theme::Theme;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::code_plugin_host::{
    CodePluginInvocation, CodePluginProductAdapter, CodePluginProductRegistration,
    ProductCodePluginHost, ProductCodePluginHostError,
};
use crate::{
    AgentDocument, HookActionDocument, HookDocument, PRODUCT_EXTENSIONS_PLUGIN_ID,
    ProductRegistration, ThemeDocument, map_duplicate_unavailable, parse_rgb, product_id,
    strict_json, validate_text,
};

pub(crate) const PRODUCT_CODE_SERVICES: &[ServiceKey] = &[
    heycode_skills::SERVICE_SKILLS,
    heycode_agent::SERVICE_COMMANDS,
    heycode_agent::SERVICE_SUBAGENTS,
    heycode_hooks::SERVICE_HOOKS,
    heycode_ui::SERVICE_UI,
    heycode_llm::SERVICE_PROVIDERS,
];

impl ProductCodePluginHost {
    pub(crate) fn production() -> Result<Self, ProductCodePluginHostError> {
        Self::new(
            PRODUCT_CODE_SERVICES,
            vec![
                Arc::new(SkillAdapter),
                Arc::new(CommandAdapter),
                Arc::new(AgentAdapter),
                Arc::new(HookAdapter),
                Arc::new(ThemeAdapter),
                Arc::new(ProviderAdapter),
            ],
        )
    }
}

impl CodePluginProductRegistration for ProductRegistration {
    fn withdraw(self: Box<Self>) {
        (*self).withdraw();
    }
}

fn inventory(contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
    let kind = match contribution.kind() {
        ContributionKind::Skill => heycode_core::ContributionKind::Skill,
        ContributionKind::Command => heycode_core::ContributionKind::Command,
        ContributionKind::Agent => heycode_core::ContributionKind::AgentPreset,
        ContributionKind::Hook => heycode_core::ContributionKind::Hook,
        ContributionKind::Theme => heycode_core::ContributionKind::Theme,
        ContributionKind::Provider => heycode_core::ContributionKind::InferenceProvider,
        ContributionKind::Mcp => heycode_core::ContributionKind::McpServer,
    };
    vec![PluginContributionSpec::new(
        kind,
        product_id(contribution.public_name()),
    )]
}

fn require_capabilities(
    invocation: &CodePluginInvocation,
    required: &[PluginPermission],
) -> Result<(), HostActivationFailure> {
    if required
        .iter()
        .all(|permission| invocation.granted_capabilities().contains(permission))
    {
        Ok(())
    } else {
        Err(HostActivationFailure::InvalidDefinition)
    }
}

struct SkillAdapter;

impl CodePluginProductAdapter for SkillAdapter {
    fn kind(&self) -> ContributionKind {
        ContributionKind::Skill
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        inventory(contribution)
    }

    fn register_inactive(
        &self,
        context: &Context,
        contribution: &CodePluginContribution,
        _invocation: CodePluginInvocation,
    ) -> Result<Box<dyn CodePluginProductRegistration>, HostActivationFailure> {
        let mut skill = Skill::parse(contribution.document()).unwrap_or_else(|| Skill {
            name: String::new(),
            description: String::new(),
            disable_model_invocation: false,
            body: contribution.document().to_owned(),
        });
        let legacy_id = product_id(contribution.public_name());
        skill.name = contribution.public_name().to_owned();
        if skill.body.trim().is_empty() || skill.body.len() > super::MAX_TEXT_BYTES {
            return Err(HostActivationFailure::InvalidDefinition);
        }
        let skills = context
            .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = skills
            .register_owned_with_aliases(skill, vec![legacy_id])
            .map_err(map_duplicate_unavailable)?;
        Ok(Box::new(ProductRegistration::Skill(registration)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeCommandDocument {
    description: String,
    timing: CodeCommandTiming,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodeCommandTiming {
    Immediate,
    Queued,
    Interrupting,
    ModelScheduling,
}

impl CodeCommandTiming {
    const fn resolve(&self) -> CommandTiming {
        match self {
            Self::Immediate => CommandTiming::Immediate,
            Self::Queued => CommandTiming::Queued,
            Self::Interrupting => CommandTiming::Interrupting,
            Self::ModelScheduling => CommandTiming::ModelScheduling,
        }
    }
}

struct CodePluginCommand {
    descriptor: CommandDescriptor,
    invocation: CodePluginInvocation,
}

#[async_trait]
impl Command for CodePluginCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let output =
            OwnedInvocation::spawn(self.invocation.clone(), "execute", json!({"args": args}))
                .await
                .map_err(anyhow::Error::new)?;
        let rendered = serde_json::to_string(&output)?;
        agent.ui().emit(heycode_agent::UiEvent::Info {
            text: format!("Code extension result: {rendered}"),
        });
        Ok(())
    }
}

struct CommandAdapter;

impl CodePluginProductAdapter for CommandAdapter {
    fn kind(&self) -> ContributionKind {
        ContributionKind::Command
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        inventory(contribution)
    }

    fn register_inactive(
        &self,
        context: &Context,
        contribution: &CodePluginContribution,
        invocation: CodePluginInvocation,
    ) -> Result<Box<dyn CodePluginProductRegistration>, HostActivationFailure> {
        let document: CodeCommandDocument = strict_json(contribution.document())?;
        validate_text(&document.description, 256)?;
        let descriptor = CommandDescriptor::new(
            product_id(contribution.public_name()),
            document.description,
            vec![
                CommandArgument::optional("input", "Optional code extension input")
                    .map_err(|_| HostActivationFailure::InvalidDefinition)?
                    .variadic(),
            ],
            document.timing.resolve(),
            CommandSource::from_plugin(PRODUCT_EXTENSIONS_PLUGIN_ID)
                .map_err(|_| HostActivationFailure::InvalidDefinition)?,
        )
        .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let commands = context
            .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = commands
            .register_owned(Arc::new(CodePluginCommand {
                descriptor,
                invocation,
            }))
            .map_err(map_duplicate_unavailable)?;
        Ok(Box::new(ProductRegistration::Command(registration)))
    }
}

struct AgentAdapter;

impl CodePluginProductAdapter for AgentAdapter {
    fn kind(&self) -> ContributionKind {
        ContributionKind::Agent
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        inventory(contribution)
    }

    fn register_inactive(
        &self,
        context: &Context,
        contribution: &CodePluginContribution,
        _invocation: CodePluginInvocation,
    ) -> Result<Box<dyn CodePluginProductRegistration>, HostActivationFailure> {
        let document: AgentDocument = strict_json(contribution.document())?;
        validate_text(&document.display, 256)?;
        validate_text(&document.instructions, super::MAX_TEXT_BYTES)?;
        let provider = document
            .provider
            .map(SubagentProviderId::new)
            .transpose()
            .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let (seed, continuation) = document.mode.resolve();
        let preset = SubagentPreset::new(
            product_id(contribution.public_name()),
            document.display,
            document.instructions,
            provider,
            seed,
            continuation,
        )
        .map_err(|_| HostActivationFailure::InvalidDefinition)?
        .with_description(document.description)
        .map_err(|_| HostActivationFailure::InvalidDefinition)?
        .with_config(document.config)
        .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let registry = context
            .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = registry
            .register_preset_owned(preset)
            .map_err(map_duplicate_unavailable)?;
        Ok(Box::new(ProductRegistration::Preset(registration)))
    }
}

struct HookAdapter;

impl CodePluginProductAdapter for HookAdapter {
    fn kind(&self) -> ContributionKind {
        ContributionKind::Hook
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        inventory(contribution)
    }

    fn register_inactive(
        &self,
        context: &Context,
        contribution: &CodePluginContribution,
        invocation: CodePluginInvocation,
    ) -> Result<Box<dyn CodePluginProductRegistration>, HostActivationFailure> {
        require_capabilities(&invocation, &[PluginPermission::HookRegistration])?;
        let document: HookDocument = strict_json(contribution.document())?;
        let action = match document.action {
            HookActionDocument::Command { command } => {
                require_capabilities(&invocation, &[PluginPermission::ProcessSpawn])?;
                validate_text(&command, super::MAX_TEXT_BYTES)?;
                HookAction::Command(command)
            }
            HookActionDocument::Prompt { prompt } => {
                validate_text(&prompt, super::MAX_TEXT_BYTES)?;
                HookAction::Prompt(prompt)
            }
            HookActionDocument::Subagent { agent, prompt } => {
                validate_text(&agent, 128)?;
                validate_text(&prompt, super::MAX_TEXT_BYTES)?;
                HookAction::Subagent { agent, prompt }
            }
            HookActionDocument::McpTool {
                server,
                tool,
                arguments,
            } => {
                require_capabilities(&invocation, &[PluginPermission::McpConnect])?;
                validate_text(&server, 128)?;
                validate_text(&tool, 128)?;
                HookAction::McpTool {
                    server,
                    tool,
                    arguments,
                }
            }
        };
        let service = context
            .get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = service.register_owned(Hook {
            owner: product_id(contribution.public_name()),
            phase: document.phase.resolve(),
            event: document.event.resolve(),
            action,
            project_scoped: document.project_scoped,
        });
        Ok(Box::new(ProductRegistration::Hook(registration)))
    }
}

struct ThemeAdapter;

impl CodePluginProductAdapter for ThemeAdapter {
    fn kind(&self) -> ContributionKind {
        ContributionKind::Theme
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        inventory(contribution)
    }

    fn register_inactive(
        &self,
        context: &Context,
        contribution: &CodePluginContribution,
        _invocation: CodePluginInvocation,
    ) -> Result<Box<dyn CodePluginProductRegistration>, HostActivationFailure> {
        let document: ThemeDocument = strict_json(contribution.document())?;
        validate_text(&document.title, 256)?;
        let theme = Theme::new(
            product_id(contribution.public_name()),
            document.title,
            [
                parse_rgb(&document.colors.accent)?,
                parse_rgb(&document.colors.success)?,
                parse_rgb(&document.colors.error)?,
                parse_rgb(&document.colors.warn)?,
                parse_rgb(&document.colors.text)?,
                parse_rgb(&document.colors.dim)?,
                parse_rgb(&document.colors.border)?,
                parse_rgb(
                    document
                        .colors
                        .code
                        .as_deref()
                        .unwrap_or(&document.colors.accent),
                )?,
                parse_rgb(
                    document
                        .colors
                        .prompt_background
                        .as_deref()
                        .unwrap_or(&document.colors.border),
                )?,
                parse_rgb(
                    document
                        .colors
                        .prompt_glyph
                        .as_deref()
                        .unwrap_or(&document.colors.dim),
                )?,
                parse_rgb(
                    document
                        .colors
                        .panel_title
                        .as_deref()
                        .unwrap_or(&document.colors.accent),
                )?,
            ],
        )
        .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let ui = context
            .get::<heycode_ui::UiRegistry>(heycode_ui::SERVICE_UI)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = ui
            .register_theme_owned(theme)
            .map_err(map_duplicate_unavailable)?;
        Ok(Box::new(ProductRegistration::Theme(registration)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeProviderDocument {
    protocol: CodeProviderProtocol,
    display_name: String,
    default_model: String,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CodeProviderProtocol {
    CodePluginV1,
}

struct CodePluginProvider {
    name: String,
    display_name: String,
    default_model: String,
    invocation: CodePluginInvocation,
}

impl Provider for CodePluginProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.clone(),
            default_model: self.default_model.clone(),
        }
    }

    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: self.name.clone(),
            display_name: self.display_name.clone(),
            protocols: vec![ProviderProtocol::OpenAiChatCompletions],
        }
    }

    fn describe_model(&self, model: &str) -> ModelDescriptor {
        ModelDescriptor::unknown(model)
    }

    fn stream(&self, request: ChatRequest) -> ChunkStream {
        let input = match provider_request(request) {
            Ok(input) => input,
            Err(error) => return Box::pin(futures::stream::once(async move { Err(error) })),
        };
        Box::pin(CodeProviderStream::new(OwnedInvocation::spawn(
            self.invocation.clone(),
            "stream",
            input,
        )))
    }
}

struct ProviderAdapter;

impl CodePluginProductAdapter for ProviderAdapter {
    fn kind(&self) -> ContributionKind {
        ContributionKind::Provider
    }

    fn inventory(&self, contribution: &CodePluginContribution) -> Vec<PluginContributionSpec> {
        inventory(contribution)
    }

    fn register_inactive(
        &self,
        context: &Context,
        contribution: &CodePluginContribution,
        invocation: CodePluginInvocation,
    ) -> Result<Box<dyn CodePluginProductRegistration>, HostActivationFailure> {
        let document: CodeProviderDocument = strict_json(contribution.document())?;
        if document.protocol != CodeProviderProtocol::CodePluginV1 {
            return Err(HostActivationFailure::Unsupported);
        }
        validate_text(&document.display_name, 256)?;
        validate_text(&document.default_model, 256)?;
        let provider = Arc::new(CodePluginProvider {
            name: product_id(contribution.public_name()),
            display_name: document.display_name,
            default_model: document.default_model,
            invocation,
        });
        let providers = context
            .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = providers
            .register_owned(provider)
            .map_err(|_| HostActivationFailure::Duplicate)?;
        Ok(Box::new(ProductRegistration::Provider(registration)))
    }
}

struct OwnedInvocation {
    cancellation: CodePluginCancellationToken,
    receiver: tokio::sync::oneshot::Receiver<Result<Value, CodePluginInvocationError>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl OwnedInvocation {
    fn spawn(invocation: CodePluginInvocation, operation: &'static str, input: Value) -> Self {
        let cancellation = CodePluginCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let worker = std::thread::spawn(move || {
            let result = invocation.invoke(operation, input, &worker_cancellation);
            let _sent = sender.send(result);
        });
        Self {
            cancellation,
            receiver,
            worker: Some(worker),
        }
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _settled = worker.join();
        }
    }
}

impl Future for OwnedInvocation {
    type Output = Result<Value, CodePluginInvocationError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.receiver).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.join();
                Poll::Ready(result.unwrap_or(Err(CodePluginInvocationError::Closed)))
            }
        }
    }
}

impl Drop for OwnedInvocation {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.join();
    }
}

fn provider_request(request: ChatRequest) -> Result<Value, LlmError> {
    let mut messages = Vec::with_capacity(request.messages.len());
    for message in request.messages {
        if !message.images.is_empty() || !message.documents.is_empty() {
            return Err(invalid_provider_response());
        }
        messages.push(message_json(message));
    }
    Ok(json!({
        "model": request.model,
        "messages": messages,
        "tools": request.tools,
        "temperature": request.temperature,
        "max_tokens": request.max_tokens,
    }))
}

fn message_json(message: ChatMessage) -> Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    json!({
        "role": role,
        "content": message.content,
        "tool_calls": message.tool_calls.map(|calls| calls.into_iter().map(|call| json!({
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        })).collect::<Vec<_>>()),
        "tool_call_id": message.tool_call_id,
        "tool_result_is_error": message.tool_result_is_error,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeProviderResult {
    chunks: Vec<CodeProviderChunk>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CodeProviderChunk {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallDelta {
        index: u16,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    Finish {
        reason: CodeProviderFinish,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodeProviderFinish {
    Stop,
    ToolCalls,
    Pause,
    Length,
}

impl CodeProviderFinish {
    const fn resolve(&self) -> FinishReason {
        match self {
            Self::Stop => FinishReason::Stop,
            Self::ToolCalls => FinishReason::ToolCalls,
            Self::Pause => FinishReason::Pause,
            Self::Length => FinishReason::Length,
        }
    }
}

enum CodeProviderStreamState {
    Invoking(OwnedInvocation),
    Ready(VecDeque<Result<StreamChunk, LlmError>>),
    Done,
}

struct CodeProviderStream {
    state: CodeProviderStreamState,
}

impl CodeProviderStream {
    fn new(invocation: OwnedInvocation) -> Self {
        Self {
            state: CodeProviderStreamState::Invoking(invocation),
        }
    }
}

impl Stream for CodeProviderStream {
    type Item = Result<StreamChunk, LlmError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        loop {
            match &mut self.state {
                CodeProviderStreamState::Invoking(invocation) => {
                    let result = match Pin::new(invocation).poll(context) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(result) => result,
                    };
                    self.state = CodeProviderStreamState::Ready(match result {
                        Ok(value) => provider_chunks(value),
                        Err(error) => VecDeque::from([Err(invocation_error(error))]),
                    });
                }
                CodeProviderStreamState::Ready(chunks) => {
                    if let Some(chunk) = chunks.pop_front() {
                        return Poll::Ready(Some(chunk));
                    }
                    self.state = CodeProviderStreamState::Done;
                }
                CodeProviderStreamState::Done => return Poll::Ready(None),
            }
        }
    }
}

fn provider_chunks(value: Value) -> VecDeque<Result<StreamChunk, LlmError>> {
    let Ok(result) = serde_json::from_value::<CodeProviderResult>(value) else {
        return VecDeque::from([Err(invalid_provider_response())]);
    };
    if result.chunks.is_empty() || result.chunks.len() > 4_096 {
        return VecDeque::from([Err(invalid_provider_response())]);
    }
    let mut output = VecDeque::with_capacity(result.chunks.len());
    let mut calls = BTreeMap::new();
    let mut usage = None;
    let last = result.chunks.len() - 1;
    for (position, chunk) in result.chunks.into_iter().enumerate() {
        let mapped = match chunk {
            CodeProviderChunk::TextDelta { text } => StreamChunk::TextDelta(text),
            CodeProviderChunk::ReasoningDelta { text } => StreamChunk::ReasoningDelta(text),
            CodeProviderChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let first = !calls.contains_key(&index);
                if first {
                    let (Some(id_value), Some(name_value)) = (id.as_ref(), name.as_ref()) else {
                        return VecDeque::from([Err(invalid_provider_response())]);
                    };
                    if !valid_provider_identity(id_value) || !valid_provider_identity(name_value) {
                        return VecDeque::from([Err(invalid_provider_response())]);
                    }
                    calls.insert(index, (id_value.clone(), name_value.clone()));
                } else if id.is_some() || name.is_some() {
                    return VecDeque::from([Err(invalid_provider_response())]);
                }
                StreamChunk::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                }
            }
            CodeProviderChunk::Usage {
                prompt_tokens,
                completion_tokens,
            } => {
                if usage.replace(position).is_some() {
                    return VecDeque::from([Err(invalid_provider_response())]);
                }
                StreamChunk::Usage(TokenUsage {
                    prompt_tokens,
                    completion_tokens,
                })
            }
            CodeProviderChunk::Finish { reason } => {
                if position != last {
                    return VecDeque::from([Err(invalid_provider_response())]);
                }
                StreamChunk::Finish(reason.resolve())
            }
        };
        output.push_back(Ok(mapped));
    }
    if !matches!(output.back(), Some(Ok(StreamChunk::Finish(_))))
        || usage.is_some_and(|position| position + 1 != last)
    {
        return VecDeque::from([Err(invalid_provider_response())]);
    }
    output
}

fn valid_provider_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn invocation_error(error: CodePluginInvocationError) -> LlmError {
    let (class, origin) = match error {
        CodePluginInvocationError::Cancelled
        | CodePluginInvocationError::Transport(CodePluginTransportFault::Cancelled) => {
            (ProviderErrorClass::Cancelled, ProviderFailureOrigin::Local)
        }
        CodePluginInvocationError::InvalidOperation
        | CodePluginInvocationError::InvalidPayload
        | CodePluginInvocationError::UnknownContribution
        | CodePluginInvocationError::Remote(CodePluginRemoteErrorCode::InvalidRequest)
        | CodePluginInvocationError::Remote(CodePluginRemoteErrorCode::Denied) => (
            ProviderErrorClass::InvalidRequest,
            ProviderFailureOrigin::Local,
        ),
        CodePluginInvocationError::Protocol(_)
        | CodePluginInvocationError::Transport(CodePluginTransportFault::Protocol) => (
            ProviderErrorClass::Protocol,
            ProviderFailureOrigin::Transport,
        ),
        CodePluginInvocationError::Closed
        | CodePluginInvocationError::RequestIdsExhausted
        | CodePluginInvocationError::Remote(CodePluginRemoteErrorCode::Unavailable)
        | CodePluginInvocationError::Remote(CodePluginRemoteErrorCode::Failed)
        | CodePluginInvocationError::Transport(CodePluginTransportFault::Spawn)
        | CodePluginInvocationError::Transport(CodePluginTransportFault::Crashed)
        | CodePluginInvocationError::Transport(CodePluginTransportFault::Unavailable) => {
            (ProviderErrorClass::Server, ProviderFailureOrigin::Local)
        }
    };
    LlmError::Provider(ProviderFailure::new(class, origin))
}

fn invalid_provider_response() -> LlmError {
    LlmError::InvalidResponse("code extension provider response is invalid".to_owned())
}
