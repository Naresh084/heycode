//! Production composition and human command adapters for session workspace changes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::workspace_transition::{
    EnterWorktreeTool, ExitWorktreeTool, SERVICE_WORKSPACE_TRANSITION, WorkspaceSnapshot,
    WorkspaceTransitionHandle, WorkspaceTransitionOrigin, WorkspaceTransitionService,
};
use heycode_agent::{
    Agent, Command, CommandArgument, CommandDescriptor, CommandSource, CommandTiming, UiEvent,
};
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor, ServiceKey,
};
use heycode_exec::{LocalShellConfig, SandboxService, ShellService, SubprocessService};
use heycode_tui::add_directory::{AddDirectoryBridge, SERVICE_ADD_DIRECTORY_PROMPT};
use tokio_util::sync::CancellationToken;

fn core(error: impl std::fmt::Display) -> CoreError {
    CoreError::other(error.to_string())
}

pub(super) fn scope_plugin(cwd: PathBuf, shell: LocalShellConfig) -> Box<dyn Plugin> {
    struct Scope {
        cwd: PathBuf,
        shell: LocalShellConfig,
    }
    impl Plugin for Scope {
        fn name(&self) -> &'static str {
            "workspace-scope"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_WORKSPACE_TRANSITION]
        }
        fn inject(&self) -> &'static [ServiceKey] {
            &[
                heycode_session::SERVICE_SESSION,
                heycode_exec::SERVICE_SANDBOX,
                heycode_exec::SERVICE_SUBPROCESS,
            ]
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let session = context
                .get::<Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| core("workspace session missing"))?;
            let (session_dir, session_id) = {
                let session = session.lock().map_err(core)?;
                (
                    session
                        .path()
                        .parent()
                        .ok_or_else(|| core("session directory missing"))?
                        .to_path_buf(),
                    session.id().to_string(),
                )
            };
            let sandbox = context
                .get::<SandboxService>(heycode_exec::SERVICE_SANDBOX)
                .ok_or_else(|| core("workspace sandbox missing"))?;
            let subprocess = context
                .get::<SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                .ok_or_else(|| core("workspace subprocess missing"))?;
            let session_dir = std::fs::canonicalize(&session_dir).map_err(core)?;
            let sessions_dir = session_dir
                .parent()
                .ok_or_else(|| core("session storage parent missing"))?;
            let configured = sessions_dir.join(format!("workspace-worktrees-{session_id}"));
            let worktrees = if configured.starts_with(&self.cwd) {
                self.cwd
                    .parent()
                    .ok_or_else(|| core("workspace root has no disjoint worktree storage parent"))?
                    .join(format!(".heycode-worktrees-{session_id}"))
            } else {
                configured
            };
            let service = WorkspaceTransitionService::open(
                session_dir.join("workspace.json"),
                self.cwd.clone(),
                (*sandbox).clone(),
                ShellService::local(self.shell.clone()),
                (*subprocess).clone(),
                worktrees,
            )
            .map_err(core)?;
            context.provide(
                SERVICE_WORKSPACE_TRANSITION,
                self.name(),
                WorkspaceTransitionHandle(service),
            )
        }
    }
    Box::new(Scope { cwd, shell })
}

pub(super) fn filesystem_plugin() -> Box<dyn Plugin> {
    struct Filesystem;
    impl Plugin for Filesystem {
        fn name(&self) -> &'static str {
            "filesystem-local"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [ServiceKey] {
            &[heycode_exec::SERVICE_FILESYSTEM]
        }
        fn inject(&self) -> &'static [ServiceKey] {
            &[SERVICE_WORKSPACE_TRANSITION]
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let owner = context
                .get::<WorkspaceTransitionHandle>(SERVICE_WORKSPACE_TRANSITION)
                .ok_or_else(|| core("workspace owner missing"))?;
            context.provide(
                heycode_exec::SERVICE_FILESYSTEM,
                self.name(),
                owner.0.filesystem(),
            )
        }
    }
    Box::new(Filesystem)
}

pub(super) fn shell_plugin() -> Box<dyn Plugin> {
    struct Shell;
    impl Plugin for Shell {
        fn name(&self) -> &'static str {
            "shell-local"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [ServiceKey] {
            &[heycode_exec::SERVICE_SHELL]
        }
        fn inject(&self) -> &'static [ServiceKey] {
            &[SERVICE_WORKSPACE_TRANSITION]
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let owner = context
                .get::<WorkspaceTransitionHandle>(SERVICE_WORKSPACE_TRANSITION)
                .ok_or_else(|| core("workspace owner missing"))?;
            context.provide(heycode_exec::SERVICE_SHELL, self.name(), owner.0.shell())
        }
    }
    Box::new(Shell)
}

pub(super) fn integration_plugin_with_imports(
    user_home: PathBuf,
    trusted: bool,
    initial_runtime: String,
    fixed_worktree_providers: bool,
    has_project_imports: bool,
) -> Box<dyn Plugin> {
    struct Integration {
        user_home: PathBuf,
        trusted: bool,
        initial_runtime: String,
        fixed_worktree_providers: bool,
        has_project_imports: bool,
    }
    impl Plugin for Integration {
        fn name(&self) -> &'static str {
            "workspace-transitions"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    PluginContributionKind::Tool,
                    PluginContributionKind::Command,
                    PluginContributionKind::Service,
                ],
            )
        }
        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_ADD_DIRECTORY_PROMPT]
        }
        fn inject(&self) -> &'static [ServiceKey] {
            &[
                SERVICE_WORKSPACE_TRANSITION,
                heycode_agent::SERVICE_AGENT,
                heycode_agent::SERVICE_COMMANDS,
                heycode_tools::SERVICE_TOOLS,
                heycode_agent::SERVICE_SUBAGENTS,
                heycode_exec::SERVICE_TERMINAL,
                heycode_exec::SERVICE_LSP,
                heycode_mcp::SERVICE_MCP,
                heycode_hooks::SERVICE_HOOKS,
                heycode_skills::SERVICE_SKILLS,
                heycode_app_server::SERVICE_APP_SERVER,
            ]
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            let mut rows = ["add-dir", "cd", "worktree"]
                .into_iter()
                .map(|name| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::Command,
                        name,
                    )
                })
                .collect::<Vec<_>>();
            rows.extend(["enter_worktree", "exit_worktree"].into_iter().map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    name,
                )
            }));
            rows
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let service = context
                .get::<WorkspaceTransitionHandle>(SERVICE_WORKSPACE_TRANSITION)
                .ok_or_else(|| core("workspace owner missing"))?
                .0
                .clone();
            let agent = context
                .get::<Agent>(heycode_agent::SERVICE_AGENT)
                .ok_or_else(|| core("workspace agent missing"))?;
            let subagents = context
                .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
                .ok_or_else(|| core("workspace child registry missing"))?;
            let terminals = context
                .get::<heycode_exec::TerminalService>(heycode_exec::SERVICE_TERMINAL)
                .ok_or_else(|| core("workspace terminals missing"))?;
            let lsp = context
                .get::<heycode_exec::LspService>(heycode_exec::SERVICE_LSP)
                .ok_or_else(|| core("workspace LSP registry missing"))?;
            let mcp = context
                .get::<heycode_mcp::McpRegistry>(heycode_mcp::SERVICE_MCP)
                .ok_or_else(|| core("workspace MCP registry missing"))?;
            let hooks = context
                .get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS)
                .ok_or_else(|| core("workspace hook registry missing"))?;
            let initial_runtime = self.initial_runtime.clone();
            let skills = context
                .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
                .ok_or_else(|| core("workspace skills registry missing"))?;
            let app_server = context
                .get::<heycode_app_server::AppServer>(heycode_app_server::SERVICE_APP_SERVER)
                .ok_or_else(|| core("workspace protocol owner missing"))?;
            let app_server = Arc::downgrade(&app_server);
            let recompose_server = app_server.clone();
            let close_server = app_server.clone();
            let fixed_worktree_providers = self.fixed_worktree_providers;
            let has_project_imports = self.has_project_imports;
            let original_root = service.original_root().to_path_buf();
            // Project declarations and configured process servers capture the
            // original composition. Their presence requires a fresh composition.
            let bound_directories = [
                ".heycode/agents",
                ".claude/agents",
                ".heycode/hooks",
                ".heycode/plugins",
            ]
            .map(|path| original_root.join(path));
            let composition_check = Arc::new(
                move |origin| -> anyhow::Result<
                    Box<dyn heycode_agent::workspace_transition::WorkspaceTransitionPermit>,
                > {
                    let protocol = app_server.upgrade().ok_or_else(|| anyhow::anyhow!("protocol owner unavailable"))?.pause_workspace(origin == WorkspaceTransitionOrigin::ModelTool).map_err(|_| anyhow::anyhow!("The protocol is active or a delegated runtime owns this session; wait for it or recompose a native session"))?;
                    anyhow::ensure!(
                        initial_runtime == "native",
                        "This session was composed for a delegated runtime; start a native session to change workspace"
                    );
                    anyhow::ensure!(
                        !has_project_imports,
                        "Imported project resources retain their original scope; recompose before changing workspace"
                    );
                    anyhow::ensure!(
                        !fixed_worktree_providers,
                        "Configured exact-base worktree providers retain their repository binding; recompose before changing workspace"
                    );
                    anyhow::ensure!(
                        lsp.servers()?.is_empty(),
                        "Configured LSP servers retain workspace state; recompose without them before changing workspace"
                    );
                    anyhow::ensure!(
                        mcp.snapshot()?.servers().is_empty(),
                        "Configured MCP servers retain workspace state; recompose without them before changing workspace"
                    );
                    anyhow::ensure!(
                        hooks.is_empty(),
                        "Configured hooks retain project trust and scope; recompose before changing workspace"
                    );
                    anyhow::ensure!(
                        !skills
                            .snapshot_records()?
                            .iter()
                            .any(|record| record.source.scope()
                                == heycode_skills::SkillSourceScope::Project),
                        "Loaded project skills retain their original source; recompose before changing workspace"
                    );
                    for path in &bound_directories {
                        if path.try_exists()? {
                            anyhow::bail!(
                                "Project-scoped declarations retain their original directory; recompose before changing workspace: {}",
                                path.display()
                            );
                        }
                    }
                    Ok(Box::new(protocol))
                },
            );
            agent
                .install_workspace(
                    service.clone(),
                    subagents,
                    (*terminals).clone(),
                    Some(self.user_home.clone()),
                    self.trusted,
                    composition_check,
                    Arc::new(move || {
                        Ok(Box::new(
                            recompose_server
                                .upgrade()
                                .ok_or_else(|| anyhow::anyhow!("protocol owner unavailable"))?
                                .pause_for_recomposition()
                                .map_err(|_| {
                                    anyhow::anyhow!(
                                        "Protocol operations must settle before recomposition"
                                    )
                                })?,
                        ))
                    }),
                    Arc::new(move || {
                        if let Some(server) = close_server.upgrade() {
                            server.close_for_recomposition();
                        }
                    }),
                )
                .map_err(core)?;
            let commands = context
                .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .ok_or_else(|| core("workspace command registry missing"))?;
            let directory_prompt = AddDirectoryBridge::default();
            context.provide(
                SERVICE_ADD_DIRECTORY_PROMPT,
                self.name(),
                directory_prompt.clone(),
            )?;
            for kind in [
                WorkspaceCommandKind::Add,
                WorkspaceCommandKind::Cd,
                WorkspaceCommandKind::Worktree,
            ] {
                commands
                    .register_effect(
                        context,
                        Arc::new(
                            WorkspaceCommand::new(kind, service.clone(), directory_prompt.clone())
                                .map_err(core)?,
                        ),
                    )
                    .map_err(core)?;
            }
            let tools = context
                .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| core("workspace tool registry missing"))?;
            for tool in [
                Arc::new(EnterWorktreeTool::new(service.clone())) as Arc<dyn heycode_tools::Tool>,
                Arc::new(ExitWorktreeTool::new(service.clone())),
            ] {
                let registration = tools.register_owned(tool).map_err(core)?;
                context.effect(move || drop(registration));
            }
            Ok(())
        }
    }
    Box::new(Integration {
        user_home,
        trusted,
        initial_runtime,
        fixed_worktree_providers,
        has_project_imports,
    })
}

#[derive(Clone, Copy)]
enum WorkspaceCommandKind {
    Add,
    Cd,
    Worktree,
}
struct WorkspaceCommand {
    kind: WorkspaceCommandKind,
    descriptor: CommandDescriptor,
    service: Arc<WorkspaceTransitionService>,
    directory_prompt: AddDirectoryBridge,
}
impl WorkspaceCommand {
    fn new(
        kind: WorkspaceCommandKind,
        service: Arc<WorkspaceTransitionService>,
        directory_prompt: AddDirectoryBridge,
    ) -> Result<Self, heycode_agent::CommandMetadataError> {
        let (id, description, arg) = match kind {
            WorkspaceCommandKind::Add => (
                "add-dir",
                "Grant a directory to this session's actual file-tool scope",
                CommandArgument::optional(
                    "path",
                    "Existing directory path; omit to choose and confirm a session-only grant",
                )?
                .variadic(),
            ),
            WorkspaceCommandKind::Cd => (
                "cd",
                "Change actual session cwd within authorized roots",
                CommandArgument::required(
                    "path",
                    "Existing directory path, absolute or relative to current cwd",
                )?
                .variadic(),
            ),
            WorkspaceCommandKind::Worktree => (
                "worktree",
                "Enter, exit, inspect or recover this session's retained Git worktree",
                CommandArgument::optional("action", "status (default), enter, exit, recover")?,
            ),
        };
        Ok(Self {
            kind,
            descriptor: CommandDescriptor::new(
                id,
                description,
                vec![arg],
                CommandTiming::Queued,
                CommandSource::from_plugin("workspace-transitions")?,
            )?,
            service,
            directory_prompt,
        })
    }
}
#[async_trait]
impl Command for WorkspaceCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let origin = WorkspaceTransitionOrigin::HumanCommand;
        let cancellation = CancellationToken::new();
        let snapshot = match self.kind {
            WorkspaceCommandKind::Add => {
                if args.trim().is_empty() {
                    return self.directory_prompt.request(self.service.clone());
                }
                self.service
                    .add_directory(&command_path(args)?, origin, cancellation)
                    .await?
            }
            WorkspaceCommandKind::Cd => {
                self.service
                    .change_directory(&command_path(args)?, origin, cancellation)
                    .await?
            }
            WorkspaceCommandKind::Worktree => match args.trim() {
                "" | "status" => self.service.snapshot()?,
                "enter" => self.service.enter_worktree(origin, cancellation).await?,
                "exit" => self.service.exit_worktree(origin, cancellation).await?,
                "recover" => self.service.recover_pending(origin, cancellation).await?,
                _ => anyhow::bail!(
                    "Usage: /worktree [status|enter|exit|recover]. Exit retains files; no force or delete option exists."
                ),
            },
        };
        agent.ui().emit(UiEvent::Info {
            text: describe(&snapshot),
        });
        Ok(())
    }
}

fn command_path(args: &str) -> anyhow::Result<PathBuf> {
    let text = args.trim();
    anyhow::ensure!(!text.is_empty(), "An existing directory path is required");
    let unquoted = if text.len() >= 2
        && ((text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'') && text.ends_with('\'')))
    {
        &text[1..text.len() - 1]
    } else {
        text
    };
    anyhow::ensure!(
        !unquoted.is_empty() && !unquoted.contains('\0'),
        "A nonempty directory path is required"
    );
    // No shell expansion/substitution: an entire path, including spaces, is data.
    Ok(Path::new(unquoted).to_path_buf())
}
fn describe(snapshot: &WorkspaceSnapshot) -> String {
    let mut text = format!(
        "Workspace: {} (revision {}). Authorized directories: {}. Project plugins and profiles remain bound to this session's original composition.",
        snapshot.cwd.display(),
        snapshot.revision,
        snapshot
            .roots
            .iter()
            .map(|root| root.path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(tree) = &snapshot.worktree {
        text.push_str(&format!(" Managed worktree: {}. /worktree exit returns to the previous scope and retains this checkout.", tree.path.display()));
    }
    if let Some(pending) = &snapshot.pending_recovery {
        text.push_str(&format!(" Recovery pending: {}. Inspect retained setup, then use /worktree recover to keep the previous scope without deletion.", pending.manager_root.display()));
    }
    if let Some(retained) = snapshot.retained_worktrees.last() {
        text.push_str(&format!(" Retained results: {}.", retained.display()));
    }
    text
}
