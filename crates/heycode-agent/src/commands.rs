//! The human command plane: slash commands executed without a model turn.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::{
    CommandArgument, CommandAvailability, CommandCatalogEntry, CommandDescriptor,
    CommandMetadataError, CommandSource, CommandTiming,
};

/// One slash command. Commands run against the [`Agent`] directly and render
/// via UI events; they never become model messages (AGENTS.md §10).
#[async_trait]
pub trait Command: Send + Sync {
    /// Stable discovery/scheduling metadata.
    fn descriptor(&self) -> &CommandDescriptor;
    /// Dynamic availability. Unavailable commands remain discoverable with a reason.
    fn availability(&self) -> CommandAvailability {
        CommandAvailability::available()
    }
    /// Execute the command.
    ///
    /// # Errors
    /// Implementation-defined; surfaced as an `UiEvent::Error`.
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()>;

    /// Execute with a caller-owned cancellation token.
    ///
    /// Existing commands inherit the exact legacy behavior. Commands that
    /// start cancellable work override this entrypoint and propagate the token
    /// through every operation they await.
    ///
    /// # Errors
    /// Implementation-defined. A cancellation-aware implementation returns a
    /// distinguishable cancellation error instead of reporting success.
    async fn execute_cancellable(
        &self,
        agent: &Agent,
        args: &str,
        _cancellation: CancellationToken,
    ) -> anyhow::Result<()> {
        self.execute(agent, args).await
    }
}

/// Command registry/discovery failure.
#[derive(Debug, thiserror::Error)]
pub enum CommandRegistryError {
    /// Descriptor validation failed while constructing built-ins/contributions.
    #[error(transparent)]
    Metadata(#[from] CommandMetadataError),
    /// One command id already has a live owner.
    #[error("command `{id}` is already registered")]
    Duplicate {
        /// Contested validated id.
        id: String,
    },
    /// Late registry mutex was poisoned.
    #[error("command registry is unavailable")]
    RegistryUnavailable,
}

/// Registry of available commands.
#[derive(Default)]
pub struct CommandRegistry {
    commands: Vec<std::sync::Arc<dyn Command>>,
    late: std::sync::Arc<std::sync::Mutex<Vec<LateCommand>>>,
}

struct LateCommand {
    command: std::sync::Arc<dyn Command>,
    token: std::sync::Arc<()>,
}

/// Exact ownership handle for one late command contribution.
///
/// Dropping this handle removes only the token-matching registration, so a
/// stale disposer cannot remove a newer command that reused the same id.
pub struct CommandRegistration {
    late: std::sync::Weak<std::sync::Mutex<Vec<LateCommand>>>,
    id: String,
    token: std::sync::Arc<()>,
    active: bool,
}

impl CommandRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register for the registry's full lifetime after publication. Duplicate
    /// names fail loud. Plugins should use [`Self::register_effect`] so their
    /// contribution is removed on rollback/shutdown.
    ///
    /// # Errors
    /// Returns an error string when the slash name is taken.
    pub fn register_shared(
        &self,
        command: std::sync::Arc<dyn Command>,
    ) -> Result<(), CommandRegistryError> {
        let _ = self.insert_late(command)?;
        Ok(())
    }

    /// Register a late command as an owning-context effect.
    ///
    /// # Errors
    /// Duplicate ids or poisoned registry state fail before the effect is
    /// published.
    pub fn register_effect(
        &self,
        context: &heycode_core::Context,
        command: std::sync::Arc<dyn Command>,
    ) -> Result<(), CommandRegistryError> {
        let registration = self.register_owned(command)?;
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Register a late command and return its exact ownership handle.
    ///
    /// This is the lower-level form used by aggregate declarative activation,
    /// whose bridge attaches the returned handle to its own context effect.
    /// Ordinary plugins should prefer [`Self::register_effect`].
    ///
    /// # Errors
    /// Duplicate ids or poisoned registry state fail before publication.
    pub fn register_owned(
        &self,
        command: std::sync::Arc<dyn Command>,
    ) -> Result<CommandRegistration, CommandRegistryError> {
        let (id, token) = self.insert_late(command)?;
        Ok(CommandRegistration {
            late: std::sync::Arc::downgrade(&self.late),
            id,
            token,
            active: true,
        })
    }

    fn insert_late(
        &self,
        command: std::sync::Arc<dyn Command>,
    ) -> Result<(String, std::sync::Arc<()>), CommandRegistryError> {
        let id = command.descriptor().id().to_owned();
        let mut late = self
            .late
            .lock()
            .map_err(|_| CommandRegistryError::RegistryUnavailable)?;
        if let Some(conflict) = self
            .commands
            .iter()
            .map(std::sync::Arc::as_ref)
            .chain(late.iter().map(|registered| registered.command.as_ref()))
            .find_map(|registered| conflicting_name(command.descriptor(), registered.descriptor()))
        {
            return Err(CommandRegistryError::Duplicate { id: conflict });
        }
        let token = std::sync::Arc::new(());
        late.push(LateCommand {
            command,
            token: token.clone(),
        });
        Ok((id, token))
    }

    /// Register a command; duplicate names fail loud.
    ///
    /// # Errors
    /// Returns an error string when the name is taken.
    pub fn register(
        &mut self,
        command: std::sync::Arc<dyn Command>,
    ) -> Result<(), CommandRegistryError> {
        let late = self
            .late
            .lock()
            .map_err(|_| CommandRegistryError::RegistryUnavailable)?;
        if let Some(conflict) = self
            .commands
            .iter()
            .map(std::sync::Arc::as_ref)
            .chain(late.iter().map(|registered| registered.command.as_ref()))
            .find_map(|registered| conflicting_name(command.descriptor(), registered.descriptor()))
        {
            return Err(CommandRegistryError::Duplicate { id: conflict });
        }
        drop(late);
        self.commands.push(command);
        Ok(())
    }

    /// Look up a command by slash name (no leading `/`).
    pub fn get(
        &self,
        name: &str,
    ) -> Result<Option<std::sync::Arc<dyn Command>>, CommandRegistryError> {
        if let Some(command) = self
            .commands
            .iter()
            .find(|command| command.descriptor().matches_name(name))
        {
            return Ok(Some(command.clone()));
        }
        let late = self
            .late
            .lock()
            .map_err(|_| CommandRegistryError::RegistryUnavailable)?;
        Ok(late
            .iter()
            .find(|registered| registered.command.descriptor().matches_name(name))
            .map(|registered| registered.command.clone()))
    }

    /// All canonical names in registration order. Aliases remain descriptor
    /// metadata and do not become duplicate inventory rows.
    pub fn names(&self) -> Result<Vec<String>, CommandRegistryError> {
        Ok(self
            .catalog()?
            .into_iter()
            .map(|entry| entry.descriptor.id().to_owned())
            .collect())
    }

    /// Descriptor plus dynamic availability for every command in deterministic
    /// registration order.
    ///
    /// # Errors
    /// Poisoned late registry state fails loud.
    pub fn catalog(&self) -> Result<Vec<CommandCatalogEntry>, CommandRegistryError> {
        let late = self
            .late
            .lock()
            .map_err(|_| CommandRegistryError::RegistryUnavailable)?;
        let mut catalog = self
            .commands
            .iter()
            .map(|command| CommandCatalogEntry {
                descriptor: command.descriptor().clone(),
                availability: command.availability(),
            })
            .collect::<Vec<_>>();
        catalog.extend(late.iter().map(|registered| CommandCatalogEntry {
            descriptor: registered.command.descriptor().clone(),
            availability: registered.command.availability(),
        }));
        Ok(catalog)
    }

    /// Help lines (`/name — help`) for every command, early and late.
    ///
    /// Late commands are included: `/help` printing only the early set once
    /// hid plugin-owned `/init`, `/skills`, `/skill` and `/plan` from users.
    pub fn help_lines(&self) -> Result<Vec<String>, CommandRegistryError> {
        Ok(self
            .catalog()?
            .into_iter()
            .map(|entry| entry.help_line())
            .collect())
    }
}

fn conflicting_name(
    candidate: &CommandDescriptor,
    registered: &CommandDescriptor,
) -> Option<String> {
    std::iter::once(candidate.id())
        .chain(candidate.aliases().iter().copied())
        .find(|name| registered.matches_name(name))
        .map(str::to_owned)
}

impl Drop for CommandRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(late) = self.late.upgrade() else {
            return;
        };
        let Ok(mut commands) = late.lock() else {
            return;
        };
        commands.retain(|registered| {
            registered.command.descriptor().id() != self.id
                || !std::sync::Arc::ptr_eq(&registered.token, &self.token)
        });
    }
}

/// Parse failures with user-facing text.
#[derive(Debug, thiserror::Error)]
pub enum CommandUsageError {
    /// The slash name is not registered.
    #[error("unknown command `{0}` — try /help")]
    Unknown(String),
}

/// Parse `input` beginning with `/` into `(name, args)`.
#[must_use]
pub fn parse_slash(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default().to_owned();
    let args = parts.next().unwrap_or_default().trim().to_owned();
    Some((name, args))
}

pub(crate) struct Help {
    descriptor: CommandDescriptor,
    registry: std::sync::OnceLock<std::sync::Weak<CommandRegistry>>,
}

#[async_trait]
impl Command for Help {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &Agent, _args: &str) -> anyhow::Result<()> {
        let selection = agent.selection();
        let providers = agent.providers().names().join(", ");
        let header = format!(
            "provider: {} ({})\nmodel: {}",
            selection.provider_name, providers, selection.model,
        );
        agent.ui().emit(crate::ui::UiEvent::HelpRequested {
            header,
            commands: self.catalog()?,
        });
        Ok(())
    }
}

impl Help {
    fn new(descriptor: CommandDescriptor) -> Self {
        Self {
            descriptor,
            registry: std::sync::OnceLock::new(),
        }
    }

    /// Point `/help` at the registry it lives in, once that registry is behind
    /// an `Arc`. Idempotent; a second call is ignored.
    pub(crate) fn bind(&self, registry: &std::sync::Arc<CommandRegistry>) {
        let _ = self.registry.set(std::sync::Arc::downgrade(registry));
    }

    /// Every registered command, or the built-in set when the registry has not
    /// been bound (a hand-built `Help` outside `builtin_commands`).
    fn catalog(&self) -> anyhow::Result<Vec<CommandCatalogEntry>> {
        if let Some(registry) = self.registry.get().and_then(std::sync::Weak::upgrade) {
            return Ok(registry.catalog()?);
        }
        Ok(vec![CommandCatalogEntry {
            descriptor: self.descriptor.clone(),
            availability: CommandAvailability::available(),
        }])
    }
}

struct Plugins {
    descriptor: CommandDescriptor,
    inventory: heycode_core::PluginInventory,
    panel: crate::UiPanelId,
}

#[async_trait]
impl Command for Plugins {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let verbose = match args.trim() {
            "" => {
                agent
                    .ui()
                    .emit(crate::ui::UiEvent::CapabilityPanelRequested {
                        panel: self.panel.clone(),
                    });
                return Ok(());
            }
            "verbose" => true,
            other => anyhow::bail!("usage: /plugins [verbose], got `{other}`"),
        };
        let snapshot = self.inventory.snapshot()?;
        let mut lines = vec!["plugins:".to_owned()];
        for applied in snapshot.plugins {
            let descriptor = applied.descriptor;
            lines.push(format!(
                "plugin {}@{} ({}; scope={})",
                descriptor.id,
                descriptor.version,
                descriptor.source.as_str(),
                applied.scope.as_str(),
            ));
            if verbose {
                lines.extend(
                    snapshot
                        .contributions
                        .iter()
                        .filter(|row| row.plugin == descriptor.id)
                        .map(|row| format!("  {}: {}", row.kind.as_str(), row.name)),
                );
            }
        }
        agent.ui().emit(crate::ui::UiEvent::Info {
            text: lines.join("\n"),
        });
        Ok(())
    }
}

struct Tools {
    descriptor: CommandDescriptor,
    inventory: heycode_core::PluginInventory,
}

#[async_trait]
impl Command for Tools {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let args = args.split_whitespace().collect::<Vec<_>>();
        let (name, json) = match args.as_slice() {
            [] => (None, false),
            ["json"] => (None, true),
            [name] => (Some(*name), false),
            [name, "json"] => (Some(*name), true),
            _ => anyhow::bail!("usage: /tools [name] [json]"),
        };
        let catalog = agent.tool_catalog(&self.inventory)?;
        let text = if let Some(name) = name {
            let row = catalog
                .tools
                .iter()
                .find(|row| row.name == name || row.aliases.iter().any(|alias| alias == name))
                .ok_or_else(|| anyhow::anyhow!("unknown registered tool `{name}`; use /tools"))?;
            if json {
                serde_json::to_string_pretty(row)?
            } else {
                let state = |value| match value {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                };
                let mut lines = vec![
                    format!("{} · {}", row.name, row.kind),
                    format!(
                        "Owner: {}",
                        if row.owners.is_empty() {
                            "unattributed".into()
                        } else {
                            row.owners.join(", ")
                        }
                    ),
                    format!(
                        "Registered: yes · prerequisites configured: {}",
                        state(row.prerequisite.configured)
                    ),
                    format!("Setup: {}", row.prerequisite.detail),
                    format!("Selected on current route: {}", state(row.route_selected)),
                    format!("Permission: {}", row.permission),
                    format!(
                        "In latest matching prepared request: {}",
                        state(row.prepared)
                    ),
                    format!(
                        "Successful session calls: {}",
                        row.successful_calls
                            .map_or_else(|| "unknown".into(), |count| count.to_string())
                    ),
                ];
                if let Some(action) = &row.last_successful_action {
                    lines.push(format!(
                        "Last successful action: {action} (other actions remain unverified)"
                    ));
                }
                if !row.aliases.is_empty() {
                    lines.push(format!("Accepted aliases: {}", row.aliases.join(", ")));
                }
                if !row.reason.is_empty() {
                    lines.push(format!("Availability: {}", row.reason));
                }
                if let Some(schema) = &row.schema {
                    lines.push(String::new());
                    lines.push(schema.description.clone());
                    if let Some(parameters) = schema
                        .parameters
                        .get("properties")
                        .and_then(serde_json::Value::as_object)
                    {
                        let required = schema
                            .parameters
                            .get("required")
                            .and_then(serde_json::Value::as_array);
                        lines.push("Parameters:".into());
                        for (name, parameter) in parameters {
                            let required = required.is_some_and(|items| {
                                items.iter().any(|item| item.as_str() == Some(name))
                            });
                            lines.push(format!(
                                "  {name} · {} · {}{}",
                                parameter
                                    .get("type")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("see schema"),
                                if required { "required" } else { "optional" },
                                parameter
                                    .get("description")
                                    .and_then(serde_json::Value::as_str)
                                    .map_or_else(String::new, |description| format!(
                                        " · {description}"
                                    ))
                            ));
                        }
                    }
                }
                lines.push(format!("\n/tools {} json shows the complete schema and evidence. Prepared metadata does not prove remote execution.",row.name));
                lines.join("\n")
            }
        } else if json {
            serde_json::to_string_pretty(&catalog)?
        } else {
            let mut lines = vec![
                format!("Tools · {} / {}", catalog.provider, catalog.model),
                format!(
                    "{} registered client tools · {} native candidates · {} prepared client schemas · enabled: unknown",
                    catalog.registered_client_count,
                    catalog.registered_native_count,
                    catalog
                        .prepared_client_count
                        .map_or_else(|| "unknown".into(), |count| count.to_string())
                ),
                String::new(),
            ];
            for row in &catalog.tools {
                let prepared = match row.prepared {
                    Some(true) => "prepared",
                    Some(false) => "not prepared",
                    None => "preparation unknown",
                };
                let owners = if row.owners.is_empty() {
                    "unattributed".into()
                } else {
                    row.owners.join(", ")
                };
                lines.push(format!(
                    "{} · {} · {} · {} successful calls · owner: {}{}",
                    row.name,
                    row.kind,
                    prepared,
                    row.successful_calls
                        .map_or_else(|| "unknown".into(), |count| count.to_string()),
                    owners,
                    if row.reason.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", row.reason)
                    }
                ));
            }
            lines.push(format!("\n{}\n/tools <name> for schema and availability stages; /tools json for the complete snapshot.", catalog.evidence));
            lines.join("\n")
        };
        agent.ui().emit(crate::ui::UiEvent::Info { text });
        Ok(())
    }
}

const COMPACT_USAGE: &str = "usage: /compact [list|strategy] [keep] [instructions...]";

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompactAction {
    List,
    Run {
        strategy: String,
        keep: u64,
        focus: Option<String>,
    },
}

struct Compact(CommandDescriptor);

impl Compact {
    async fn execute_with_cancellation(
        &self,
        agent: &Agent,
        args: &str,
        cancellation: CancellationToken,
    ) -> anyhow::Result<()> {
        match parse_compact_action(args, &agent.compaction_strategies())? {
            CompactAction::List => {
                let rows = agent
                    .compaction_strategies()
                    .into_iter()
                    .map(|strategy| {
                        format!("{} ({})", strategy.id().as_str(), strategy.kind().name())
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                agent.ui().emit(crate::ui::UiEvent::Info {
                    text: format!("compaction strategies\n{rows}"),
                });
            }
            CompactAction::Run {
                strategy,
                keep,
                focus,
            } => {
                match agent
                    .compact_with_focus(&strategy, keep, focus.as_deref(), cancellation)
                    .await
                {
                    Ok(crate::CompactionOutcome::Noop { .. }) => {
                        agent.ui().emit(crate::ui::UiEvent::Info {
                            text: "nothing to compact yet".to_owned(),
                        })
                    }
                    Ok(crate::CompactionOutcome::Applied { .. }) => {}
                    Err(error @ crate::CompactionError::Cancelled) => return Err(error.into()),
                    Err(err) => agent.ui().emit(crate::ui::UiEvent::Error {
                        message: err.to_string(),
                    }),
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Command for Compact {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.0
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        self.execute_with_cancellation(agent, args, CancellationToken::new())
            .await
    }

    async fn execute_cancellable(
        &self,
        agent: &Agent,
        args: &str,
        cancellation: CancellationToken,
    ) -> anyhow::Result<()> {
        self.execute_with_cancellation(agent, args, cancellation)
            .await
    }
}

fn parse_compact_action(
    args: &str,
    strategies: &[crate::CompactionStrategyDescriptor],
) -> anyhow::Result<CompactAction> {
    let input = args.trim();
    if input.is_empty() {
        return Ok(CompactAction::Run {
            strategy: crate::PortableCompaction::ID.to_owned(),
            keep: crate::compact::DEFAULT_KEEP_TURNS,
            focus: None,
        });
    }
    let (first, after_first) = split_first_token(input);
    if first == "list" {
        if !after_first.is_empty() {
            anyhow::bail!(COMPACT_USAGE);
        }
        return Ok(CompactAction::List);
    }

    let known_strategy = strategies
        .iter()
        .any(|strategy| strategy.id().as_str() == first);
    if known_strategy {
        let (keep, remainder) = match split_optional_token(after_first) {
            Some((candidate, remainder)) if looks_like_integer(candidate) => {
                (positive_keep(candidate)?, remainder)
            }
            _ => (crate::compact::DEFAULT_KEEP_TURNS, after_first),
        };
        return Ok(CompactAction::Run {
            strategy: first.to_owned(),
            keep,
            focus: parse_compact_focus(remainder)?,
        });
    }

    if looks_like_integer(first) {
        return Ok(CompactAction::Run {
            strategy: crate::PortableCompaction::ID.to_owned(),
            keep: positive_keep(first)?,
            focus: parse_compact_focus(after_first)?,
        });
    }

    Ok(CompactAction::Run {
        strategy: crate::PortableCompaction::ID.to_owned(),
        keep: crate::compact::DEFAULT_KEEP_TURNS,
        focus: parse_compact_focus(input)?,
    })
}

fn split_optional_token(input: &str) -> Option<(&str, &str)> {
    (!input.is_empty()).then(|| split_first_token(input))
}

fn split_first_token(input: &str) -> (&str, &str) {
    let input = input.trim_start();
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    (&input[..end], input[end..].trim_start())
}

fn looks_like_integer(value: &str) -> bool {
    let digits = value
        .strip_prefix('+')
        .or_else(|| value.strip_prefix('-'))
        .unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_compact_focus(input: &str) -> anyhow::Result<Option<String>> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let (first, remainder) = split_first_token(input);
    if first == "--" {
        if remainder.is_empty() {
            anyhow::bail!("compact focus instructions cannot be empty");
        }
        Ok(Some(remainder.to_owned()))
    } else {
        Ok(Some(input.to_owned()))
    }
}

fn positive_keep(value: &str) -> anyhow::Result<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|keep| *keep > 0)
        .ok_or_else(|| anyhow::anyhow!("compact keep must be a positive integer"))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module, clippy::panic, clippy::unwrap_used)]
mod compact_parser_tests {
    use super::*;

    fn strategies() -> Vec<crate::CompactionStrategyDescriptor> {
        [
            (
                crate::PortableCompaction::ID,
                crate::CompactionKind::Portable,
            ),
            (crate::NativeCompaction::ID, crate::CompactionKind::Native),
            (crate::PruneCompaction::ID, crate::CompactionKind::Prune),
        ]
        .into_iter()
        .map(|(id, kind)| {
            crate::CompactionStrategyDescriptor::new(
                crate::CompactionStrategyId::new(id).unwrap(),
                kind,
            )
        })
        .collect()
    }

    fn run(args: &str) -> (String, u64, Option<String>) {
        match parse_compact_action(args, &strategies()).unwrap() {
            CompactAction::Run {
                strategy,
                keep,
                focus,
            } => (strategy, keep, focus),
            CompactAction::List => panic!("expected a compaction run"),
        }
    }

    #[test]
    fn legacy_compact_forms_keep_their_meaning() {
        assert_eq!(
            run(""),
            (
                crate::PortableCompaction::ID.to_owned(),
                crate::compact::DEFAULT_KEEP_TURNS,
                None,
            )
        );
        assert_eq!(
            run("7"),
            (crate::PortableCompaction::ID.to_owned(), 7, None)
        );
        assert_eq!(
            run("prune-oldest"),
            (
                crate::PruneCompaction::ID.to_owned(),
                crate::compact::DEFAULT_KEEP_TURNS,
                None,
            )
        );
        assert_eq!(
            run("portable-summary 3"),
            (crate::PortableCompaction::ID.to_owned(), 3, None)
        );
        assert_eq!(
            parse_compact_action("list", &strategies()).unwrap(),
            CompactAction::List
        );
    }

    #[test]
    fn natural_and_structural_focus_forms_are_unambiguous() {
        let default = crate::compact::DEFAULT_KEEP_TURNS;
        assert_eq!(
            run("preserve API decisions and unfinished migrations"),
            (
                crate::PortableCompaction::ID.to_owned(),
                default,
                Some("preserve API decisions and unfinished migrations".to_owned()),
            )
        );
        assert_eq!(
            run("-- preserve exact command output"),
            (
                crate::PortableCompaction::ID.to_owned(),
                default,
                Some("preserve exact command output".to_owned()),
            )
        );
        assert_eq!(
            run("4 -- focus on cancellation semantics"),
            (
                crate::PortableCompaction::ID.to_owned(),
                4,
                Some("focus on cancellation semantics".to_owned()),
            )
        );
        assert_eq!(
            run("portable-summary -- focus on the API"),
            (
                crate::PortableCompaction::ID.to_owned(),
                default,
                Some("focus on the API".to_owned()),
            )
        );
        assert_eq!(
            run("portable-summary 2 -- focus on the API"),
            (
                crate::PortableCompaction::ID.to_owned(),
                2,
                Some("focus on the API".to_owned()),
            )
        );
    }

    #[test]
    fn invalid_structural_forms_fail_loudly() {
        for input in ["0", "+0", "-1", "18446744073709551616"] {
            let error = parse_compact_action(input, &strategies()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("compact keep must be a positive integer"),
                "{input}: {error}"
            );
        }
        for input in ["--", "portable-summary --", "portable-summary 2 --"] {
            let error = parse_compact_action(input, &strategies()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("compact focus instructions cannot be empty"),
                "{input}: {error}"
            );
        }
        assert_eq!(
            parse_compact_action("list now", &strategies())
                .unwrap_err()
                .to_string(),
            COMPACT_USAGE
        );
    }
}

struct TitleCommand(CommandDescriptor);
#[async_trait]
impl Command for TitleCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.0
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let arg = args.trim();
        if arg.is_empty() {
            let s = agent.session().lock().unwrap_or_else(|e| e.into_inner());
            let current = crate::title::current(s.events());
            agent.ui().emit(crate::ui::UiEvent::Info {
                text: current
                    .map(|t| format!("title: {t}"))
                    .unwrap_or_else(|| "no title yet".to_owned()),
            });
            return Ok(());
        }
        crate::title::set(agent.session(), arg.to_owned())
            .await
            .map_err(|e| anyhow::anyhow!("failed to set title: {e}"))?;
        agent.ui().emit(crate::ui::UiEvent::Info {
            text: format!("titled: {arg}"),
        });
        Ok(())
    }
}

struct Quit(CommandDescriptor);
#[async_trait]
impl Command for Quit {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.0
    }
    async fn execute(&self, agent: &Agent, _args: &str) -> anyhow::Result<()> {
        agent.ui().emit(crate::ui::UiEvent::QuitRequested);
        Ok(())
    }
}

/// The built-in registry plus a handle to its `/help`, so the caller can bind
/// help to the published `Arc<CommandRegistry>` and let it enumerate late
/// (`register_shared`) commands.
///
/// # Errors
/// Duplicate command name.
pub(crate) fn builtin_commands_with_help(
    inventory: heycode_core::PluginInventory,
) -> Result<(CommandRegistry, std::sync::Arc<Help>), CommandRegistryError> {
    let mut registry = CommandRegistry::new();
    let source = CommandSource::from_plugin("commands")?;
    let help = std::sync::Arc::new(Help::new(CommandDescriptor::new(
        "help",
        "Show available commands",
        Vec::new(),
        CommandTiming::Immediate,
        source.clone(),
    )?));
    for cmd in [
        help.clone() as std::sync::Arc<dyn Command>,
        std::sync::Arc::new(Plugins {
            descriptor: CommandDescriptor::new(
                "plugins",
                "Open plugins; `verbose` prints attributed contributions",
                vec![CommandArgument::optional(
                    "verbose",
                    "Use `verbose` for exact rows",
                )?],
                CommandTiming::Immediate,
                source.clone(),
            )?,
            inventory: inventory.clone(),
            panel: crate::UiPanelId::new("plugins")
                .map_err(|_| CommandMetadataError::InvalidId { field: "panel id" })?,
        }),
        std::sync::Arc::new(Tools {
            descriptor: CommandDescriptor::new(
                "tools",
                "Inspect tool registration, setup, request and execution evidence",
                vec![
                    CommandArgument::optional("name", "Canonical tool name or alias")?,
                    CommandArgument::optional("json", "Print structured inventory")?,
                ],
                CommandTiming::Immediate,
                source.clone(),
            )?,
            inventory,
        }),
        std::sync::Arc::new(Compact(CommandDescriptor::new(
            "compact",
            "List or run a composed compaction strategy",
            vec![
                CommandArgument::optional("strategy", "Strategy id or `list`")?,
                CommandArgument::optional("keep", "Recent turn count to preserve")?,
                CommandArgument::optional("instructions", "Human focus for portable-summary")?
                    .variadic(),
            ],
            CommandTiming::ModelScheduling,
            source.clone(),
        )?)),
        std::sync::Arc::new(TitleCommand(CommandDescriptor::new(
            "title",
            "Show or set the session title",
            vec![CommandArgument::optional("text", "New session title")?.variadic()],
            CommandTiming::Queued,
            source.clone(),
        )?)),
        std::sync::Arc::new(Quit(CommandDescriptor::new(
            "quit",
            "Exit heycode",
            Vec::new(),
            CommandTiming::Interrupting,
            source,
        )?)),
    ] {
        registry.register(cmd)?;
    }
    crate::session_control::register(&mut registry)?;
    Ok((registry, help))
}
