//! Read-only skill usage and context diagnostics from admitted catalog and log.
use crate::{SkillAdmission, SkillSet};
use heycode_agent::{
    Agent, Command, CommandDescriptor, CommandRegistry, CommandSource, CommandTiming,
};
use heycode_core::{Context, CoreError, CoreResult};
use std::sync::Arc;

/// One source-attributed row in the native Skill Stats panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDoctorRow {
    name: String,
    source: String,
    admission: SkillAdmission,
    context_tokens: Option<usize>,
    session_uses: usize,
    retained_in_context: usize,
    body_bytes: usize,
    body_tokens: usize,
    missing_description: bool,
    empty_instructions: bool,
}

impl SkillDoctorRow {
    /// Stable skill name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Trust scope that admitted this skill.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Current four-state admission.
    pub const fn admission(&self) -> SkillAdmission {
        self.admission
    }

    /// Approximate tokens in the current per-request catalog listing. `None`
    /// means this skill is excluded from the model-visible listing.
    pub const fn context_tokens(&self) -> Option<usize> {
        self.context_tokens
    }

    /// Successful instruction-body deliveries observed in this session.
    pub const fn session_uses(&self) -> usize {
        self.session_uses
    }

    /// Delivered bodies still present in the repaired current context.
    pub const fn retained_in_context(&self) -> usize {
        self.retained_in_context
    }

    /// Bounded body size captured in the trusted catalog snapshot.
    pub const fn body_bytes(&self) -> usize {
        self.body_bytes
    }

    /// Approximate body tokens (Unicode scalar count divided by four).
    pub const fn body_tokens(&self) -> usize {
        self.body_tokens
    }

    /// Whether model discovery lacks a useful description.
    pub const fn missing_description(&self) -> bool {
        self.missing_description
    }

    /// Whether the instruction body is empty after trimming.
    pub const fn empty_instructions(&self) -> bool {
        self.empty_instructions
    }
}

/// Complete native Skill Stats snapshot. Historical fields which heycode does
/// not durably collect stay explicitly unavailable rather than being inferred
/// from current-session events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDoctorSnapshot {
    rows: Vec<SkillDoctorRow>,
    skipped: Vec<String>,
    catalog_tokens: usize,
}

impl SkillDoctorSnapshot {
    /// Build diagnostics from the exact admitted catalog and current durable
    /// session log without scheduling inference or reading provider billing.
    pub fn capture(
        skills: &SkillSet,
        events: &[heycode_session::SessionEvent],
    ) -> Result<Self, crate::SkillRegistryError> {
        let catalog = skills.catalog_snapshot()?;
        let messages = heycode_session::derive_messages_repaired(events);
        let mut catalog_chars = 0_usize;
        let rows = catalog
            .records()
            .iter()
            .map(|catalog_row| {
                let record = catalog_row.record();
                let skill = &record.skill;
                let marker = format!("<skill name=\"{}\">", skill.name);
                let retained_in_context = messages
                    .iter()
                    .filter(|message| {
                        message.tool_result_is_error != Some(true)
                            && message.content.starts_with(&marker)
                    })
                    .count();
                let session_uses = events
                    .iter()
                    .filter(|event| match &event.kind {
                        heycode_session::SessionEventKind::ToolResult {
                            content, is_error, ..
                        } => !is_error && content.starts_with(&marker),
                        heycode_session::SessionEventKind::UserMessage { text } => {
                            text.starts_with(&marker)
                        }
                        _ => false,
                    })
                    .count();
                let context_tokens = catalog_row.model_invocable().then(|| {
                    let mut characters = skill.name.chars().count().saturating_add(5);
                    if catalog_row.admission().description_visible() {
                        characters = characters.saturating_add(skill.description.chars().count());
                    }
                    catalog_chars = catalog_chars.saturating_add(characters);
                    characters.div_ceil(4)
                });
                SkillDoctorRow {
                    name: skill.name.clone(),
                    source: record.source.scope().as_str().to_owned(),
                    admission: catalog_row.admission(),
                    context_tokens,
                    session_uses,
                    retained_in_context,
                    body_bytes: skill.body.len(),
                    body_tokens: skill.body.chars().count().div_ceil(4),
                    missing_description: skill.description.is_empty(),
                    empty_instructions: skill.body.trim().is_empty(),
                }
            })
            .collect();
        let skipped = skills
            .skipped()
            .into_iter()
            .map(|row| format!("{}/{}: {}", row.root, row.directory, row.reason))
            .collect();
        Ok(Self {
            rows,
            skipped,
            catalog_tokens: catalog_chars.div_ceil(4),
        })
    }

    /// Rows in deterministic catalog order.
    pub fn rows(&self) -> &[SkillDoctorRow] {
        &self.rows
    }

    /// Invalid or unavailable discovery candidates.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Approximate combined listing cost for each native request.
    pub const fn catalog_tokens(&self) -> usize {
        self.catalog_tokens
    }

    /// Seven-day provider usage history is not persisted by heycode today.
    pub const fn seven_day_history_available(&self) -> bool {
        false
    }
}

struct SkillDoctor {
    skills: SkillSet,
    descriptor: CommandDescriptor,
}
#[async_trait::async_trait]
impl Command for SkillDoctor {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        if !args.trim().is_empty() {
            anyhow::bail!("usage: /skill-doctor");
        }
        // The TUI captures the current catalog and durable log only after it
        // receives this request, so opening the panel cannot leave transcript
        // text behind or race a later session replay.
        let _ = self.skills.catalog_snapshot()?;
        agent
            .ui()
            .emit(heycode_agent::UiEvent::CapabilityPanelRequested {
                panel: heycode_agent::UiPanelId::new("skill-doctor")?,
            });
        Ok(())
    }
}

pub(crate) fn register(
    ctx: &Context,
    commands: &CommandRegistry,
    skills: SkillSet,
) -> CoreResult<()> {
    let descriptor = CommandDescriptor::new(
        "skill-doctor",
        "Inspect skill discovery, loading and context costs",
        Vec::new(),
        CommandTiming::Immediate,
        CommandSource::from_plugin("skills").map_err(|e| CoreError::other(e.to_string()))?,
    )
    .map_err(|e| CoreError::other(e.to_string()))?;
    commands
        .register_effect(ctx, Arc::new(SkillDoctor { skills, descriptor }))
        .map_err(|e| CoreError::other(e.to_string()))
}
