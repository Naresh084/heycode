//! Plugin constructors for the agent capability.

use std::sync::Arc;

use heycode_core::{Context, CoreError, CoreResult, Plugin};

use crate::agent::Agent;
use crate::approval::ApprovalPolicy;
use crate::commands::CommandRegistry;
use crate::{
    SERVICE_AGENT, SERVICE_AGENT_OPTIONS, SERVICE_APPROVAL, SERVICE_APPROVAL_INTERACTIVE,
    SERVICE_APPROVAL_SWITCH, SERVICE_COMMANDS, SERVICE_PLAN,
};

/// Provide service `"approval"` with the given policy.
pub fn approval_plugin(policy: Arc<dyn ApprovalPolicy>) -> Box<dyn Plugin> {
    struct ApprovalPlugin(Arc<dyn ApprovalPolicy>);
    impl Plugin for ApprovalPlugin {
        fn name(&self) -> &'static str {
            "approval"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "approval",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_APPROVAL]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            ctx.provide(SERVICE_APPROVAL, "approval", ApprovalHandle(self.0.clone()))
        }
    }
    Box::new(ApprovalPlugin(policy))
}

/// Provide service `"commands"` preloaded with the built-ins.
pub fn commands_plugin() -> Box<dyn Plugin> {
    struct CommandsPlugin;
    impl Plugin for CommandsPlugin {
        fn name(&self) -> &'static str {
            "commands"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "commands",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            [
                "help",
                "plugins",
                "tools",
                "compact",
                "title",
                "quit",
                "recap",
                "btw",
                "output-style",
                "questions",
                "answer",
            ]
            .into_iter()
            .map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    name,
                )
            })
            .collect()
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_COMMANDS]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let (registry, help) =
                crate::commands::builtin_commands_with_help(ctx.plugin_inventory())
                    .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.provide(SERVICE_COMMANDS, "commands", registry)?;
            // Bind AFTER publication so `/help` can enumerate late registrations
            // (`/skills`, `/skill`, `/plan`) through the shared Arc.
            let published = ctx
                .get::<CommandRegistry>(SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("commands missing after provide"))?;
            help.bind(&published);
            Ok(())
        }
    }
    Box::new(CommandsPlugin)
}

/// Knobs threaded from config into the agent plugin.
#[derive(Clone, Default)]
pub struct AgentOptions {
    /// Pressure policy for automatic compaction.
    pub compaction: crate::agent::CompactionPolicy,
    /// Maximum nesting depth of `task` subagent calls.
    pub max_task_depth: u32,
    /// Working-directory override; falls back to the process cwd.
    pub cwd: Option<std::path::PathBuf>,
    /// Generate a short session title after the first completed turn.
    pub auto_title: bool,
}

/// Publish one resolved `agent-options` service.
#[must_use]
pub fn agent_options_plugin(options: AgentOptions) -> Box<dyn Plugin> {
    struct AgentOptionsPlugin(AgentOptions);
    impl Plugin for AgentOptionsPlugin {
        fn name(&self) -> &'static str {
            "agent-options"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "agent-options",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AGENT_OPTIONS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context.provide(SERVICE_AGENT_OPTIONS, self.name(), self.0.clone())
        }
    }
    Box::new(AgentOptionsPlugin(options))
}

/// Provide service `"agent"` from the composed world.
///
/// Injects: `session`, `providers`, `models`, `llm`, `tools`, `seam/pre_tool`,
/// `prompt`, `approval`, `agent-options`, `native-tools`, `token-counters`,
/// `compactions`.
pub fn agent_plugin() -> Box<dyn Plugin> {
    agent_plugin_with_job_limits(crate::JobLimits::default())
}

/// Compose the native Agent with explicit global job admission/history limits.
pub fn agent_plugin_with_job_limits(limits: crate::JobLimits) -> Box<dyn Plugin> {
    struct AgentPlugin(crate::JobLimits);
    impl Plugin for AgentPlugin {
        fn name(&self) -> &'static str {
            "agent"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "agent",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Tool,
                    heycode_core::PluginContributionKind::Waterfall,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            let mut rows = [
                "ask_user_question",
                "ask_user_question_async",
                "list_jobs",
                "cancel_job",
            ]
            .into_iter()
            .map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    name,
                )
            })
            .collect::<Vec<_>>();
            rows.extend([
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::InterceptionLayer,
                    "provider/request:authentication",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::InterceptionLayer,
                    "provider/request:native-tools",
                ),
            ]);
            rows
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_session::SERVICE_SESSION,
                heycode_llm::SERVICE_PROVIDERS,
                heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
                heycode_llm::SERVICE_MODELS,
                heycode_llm::SERVICE_LLM,
                heycode_llm::SERVICE_TOKEN_COUNTERS,
                crate::SERVICE_COMPACTIONS,
                heycode_native_tools::SERVICE_NATIVE_TOOLS,
                heycode_tools::SERVICE_TOOLS,
                heycode_tools::SEAM_PRE_TOOL,
                heycode_prompt::SERVICE_PROMPT,
                SERVICE_APPROVAL,
                SERVICE_AGENT_OPTIONS,
            ]
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AGENT, crate::SERVICE_JOBS, crate::SERVICE_QUESTIONS]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let session = ctx
                .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session service missing"))?;
            let providers = ctx
                .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
                .ok_or_else(|| CoreError::other("providers service missing"))?;
            let provider_interception = ctx
                .get::<heycode_llm::ProviderInterception>(
                    heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
                )
                .ok_or_else(|| CoreError::other("provider interception service missing"))?;
            let catalogs = ctx
                .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
                .ok_or_else(|| CoreError::other("models service missing"))?;
            let selection = ctx
                .get::<heycode_llm::LlmSelection>(heycode_llm::SERVICE_LLM)
                .ok_or_else(|| CoreError::other("llm selection missing"))?;
            let token_counters = ctx
                .get::<heycode_llm::TokenCounterRegistry>(heycode_llm::SERVICE_TOKEN_COUNTERS)
                .ok_or_else(|| CoreError::other("token-counters service missing"))?;
            let compactions = ctx
                .get::<crate::CompactionRegistry>(crate::SERVICE_COMPACTIONS)
                .ok_or_else(|| CoreError::other("compactions service missing"))?;
            let native_tools = ctx
                .get::<heycode_native_tools::NativeToolRegistry>(
                    heycode_native_tools::SERVICE_NATIVE_TOOLS,
                )
                .ok_or_else(|| CoreError::other("native-tools service missing"))?;
            let tools = ctx
                .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tools service missing"))?;
            let tools_for_jobs = tools.clone();
            let questions = crate::InteractiveQuestion::new();
            let question_registration = tools
                .register_owned(Arc::new(
                    crate::interactive_question::AskUserQuestionTool::new(questions.clone()),
                ))
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.effect(move || drop(question_registration));
            ctx.provide(crate::SERVICE_QUESTIONS, self.name(), questions)?;
            let pre_seam = ctx
                .get::<heycode_core::Waterfall<heycode_tools::PreToolDecision>>(
                    heycode_tools::SEAM_PRE_TOOL,
                )
                .ok_or_else(|| CoreError::other("pre-tool seam missing"))?;
            let prompt = ctx
                .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
                .ok_or_else(|| CoreError::other("prompt service missing"))?;
            let approval = ctx
                .get::<ApprovalHandle>(SERVICE_APPROVAL)
                .ok_or_else(|| CoreError::other("approval service missing"))?;
            let options = ctx
                .get::<AgentOptions>(SERVICE_AGENT_OPTIONS)
                .map(|o| (*o).clone())
                .ok_or_else(|| CoreError::other("agent-options service missing"))?;
            let cwd = options
                .cwd
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            provider_interception.register_request(
                ctx,
                crate::provider_consumers::AuthenticationRequestLayer::new(providers.clone()),
            );
            provider_interception.register_request(
                ctx,
                crate::provider_consumers::NativeToolRequestLayer::new(native_tools.clone()),
            );
            let agent = Agent::new(
                session,
                providers,
                provider_interception,
                catalogs,
                compactions,
                token_counters,
                native_tools,
                (*selection).clone(),
                tools,
                pre_seam,
                prompt,
                approval.0.clone(),
                ctx.get::<crate::plan::PlanHandle>(SERVICE_PLAN),
                options.auto_title,
                cwd,
                options.compaction,
                ctx.events.clone(),
            );
            let shutdown = agent.token();
            ctx.provide(SERVICE_AGENT, "agent", agent)?;
            ctx.effect(move || shutdown.shutdown());
            // Background jobs live beside the Agent because a settlement
            // delivers into its durable inbox and its turn settlement is what
            // replenishes the wake budget.
            ctx.provide(
                crate::SERVICE_JOBS,
                "agent",
                std::sync::Arc::new(crate::JobRegistry::with_limits(0, self.0)),
            )?;
            let jobs = ctx
                .get::<std::sync::Arc<crate::JobRegistry>>(crate::SERVICE_JOBS)
                .ok_or_else(|| CoreError::other("job registry missing"))?;
            let feature_agent = ctx
                .get::<Agent>(SERVICE_AGENT)
                .ok_or_else(|| CoreError::other("agent missing for optional questions"))?;
            let async_registration = tools_for_jobs
                .register_owned(Arc::new(crate::session_control::AsyncQuestionTool(
                    Arc::downgrade(&feature_agent),
                )))
                .map_err(|error| CoreError::other(error.to_string()))?;
            ctx.effect(move || drop(async_registration));
            let live_agent = ctx
                .get::<crate::Agent>(SERVICE_AGENT)
                .ok_or_else(|| CoreError::other("agent service missing"))?;
            let history = live_agent
                .session()
                .lock()
                .map_err(|_| CoreError::other("session unavailable"))?
                .path()
                .parent()
                .ok_or_else(|| CoreError::other("session directory missing"))?
                .join("jobs.json");
            jobs.attach_history(history)
                .map_err(|error| CoreError::other(error.to_string()))?;
            live_agent.install_jobs((*jobs).clone());
            let disposable = jobs.clone();
            ctx.effect(move || disposable.dispose());
            for tool in [
                Arc::new(crate::jobs::ListJobsTool::new((*jobs).clone()))
                    as Arc<dyn heycode_tools::Tool>,
                Arc::new(crate::jobs::CancelJobTool::new((*jobs).clone())),
            ] {
                tools_for_jobs
                    .register_shared(tool)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            Ok(())
        }
    }
    Box::new(AgentPlugin(limits))
}

/// Newtype so `Arc<dyn ApprovalPolicy>` can ride the type-erased service map.
pub struct ApprovalHandle(pub Arc<dyn ApprovalPolicy>);

/// Service value behind `"approval-switch"`: the live mode switch.
pub struct ApprovalSwitchHandle(pub Arc<crate::SwitchableApproval>);

/// Provide `"approval"` (as the interactive policy) AND the concrete
/// `"approval-interactive"` service so front ends can answer dialogs.
pub fn interactive_approval_plugin() -> Box<dyn Plugin> {
    interactive_approval_plugin_inner(None)
}

/// Provide interactive approval using a caller-shared policy handle.
///
/// Activation rebinds the handle to the composed Context event bus before
/// publication, so other product adapters can share the exact policy without
/// losing the ordinary Agent/TUI approval event route.
#[must_use]
pub fn interactive_approval_plugin_with_policy(
    policy: crate::InteractiveApproval,
) -> Box<dyn Plugin> {
    interactive_approval_plugin_inner(Some(policy))
}

/// The interactive surface's approval plugin: publishes one shared
/// [`crate::SwitchableApproval`] as `approval` plus an `approval-switch` so
/// `/permissions <mode>` can change it live, and the interactive instance it
/// prompts through as `approval-interactive`.
pub fn switchable_approval_plugin(
    switch: Arc<crate::SwitchableApproval>,
    interactive: crate::InteractiveApproval,
) -> Box<dyn Plugin> {
    struct SwitchablePlugin {
        switch: Arc<crate::SwitchableApproval>,
        interactive: crate::InteractiveApproval,
    }
    impl Plugin for SwitchablePlugin {
        fn name(&self) -> &'static str {
            "approval-ask"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "approval-ask",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                SERVICE_APPROVAL,
                SERVICE_APPROVAL_INTERACTIVE,
                SERVICE_APPROVAL_SWITCH,
            ]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            self.interactive.bind_bus(ctx.events.clone());
            ctx.provide(
                SERVICE_APPROVAL,
                "approval-ask",
                ApprovalHandle(self.switch.clone() as Arc<dyn ApprovalPolicy>),
            )?;
            ctx.provide(
                SERVICE_APPROVAL_INTERACTIVE,
                "approval-ask",
                self.interactive.clone(),
            )?;
            ctx.provide(
                SERVICE_APPROVAL_SWITCH,
                "approval-ask",
                ApprovalSwitchHandle(self.switch.clone()),
            )
        }
    }
    Box::new(SwitchablePlugin {
        switch,
        interactive,
    })
}

fn interactive_approval_plugin_inner(
    policy: Option<crate::InteractiveApproval>,
) -> Box<dyn Plugin> {
    struct AskPlugin(Option<crate::InteractiveApproval>);
    impl Plugin for AskPlugin {
        fn name(&self) -> &'static str {
            "approval-ask"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "approval-ask",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_APPROVAL, SERVICE_APPROVAL_INTERACTIVE]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let policy = self
                .0
                .clone()
                .unwrap_or_else(|| crate::InteractiveApproval::new(ctx.events.clone()));
            policy.bind_bus(ctx.events.clone());
            ctx.provide(
                SERVICE_APPROVAL,
                "approval-ask",
                crate::plugin::ApprovalHandle(Arc::new(policy.clone())),
            )?;
            ctx.provide(SERVICE_APPROVAL_INTERACTIVE, "approval-ask", policy)
        }
    }
    Box::new(AskPlugin(policy))
}
