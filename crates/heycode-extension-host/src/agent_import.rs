//! Explicit competitor import; never discard an unsupported policy field.
use heycode_agent::{ChildPermissions, SubagentConfig};
use serde::Deserialize;

use crate::{AgentDocument, AgentModeDocument, MAX_TEXT_BYTES};

/// Supported standalone custom-agent formats.
#[derive(Debug, Clone, Copy)]
pub enum AgentImportFormat {
    /// Claude Markdown body with YAML front matter.
    Claude,
    /// Codex standalone agent TOML.
    Codex,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexAgent {
    name: String,
    description: String,
    developer_instructions: String,
    model: Option<String>,
    model_reasoning_effort: Option<String>,
    sandbox_mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeAgent {
    name: String,
    description: String,
    model: Option<String>,
    effort: Option<String>,
    tools: Option<Names>,
    #[serde(rename = "disallowedTools", default)]
    denied_tools: Option<Names>,
    #[serde(rename = "permissionMode")]
    permissions: Option<String>,
    #[serde(rename = "maxTurns")]
    max_turns: Option<u32>,
    memory: Option<heycode_agent::ChildMemory>,
    isolation: Option<heycode_agent::ChildIsolation>,
    background: Option<bool>,
    #[serde(rename = "mcpServers")]
    mcp_servers: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Names {
    List(Vec<String>),
    Csv(String),
}
impl Names {
    fn tools(self) -> Result<Vec<String>, String> {
        let names = match self {
            Self::List(names) => names,
            Self::Csv(text) => text.split(',').map(|s| s.trim().to_owned()).collect(),
        };
        names
            .into_iter()
            .map(|name| {
                Ok(match name.as_str() {
                    "Read" => "read",
                    "Write" => "write",
                    "Edit" => "edit",
                    "Bash" => "bash",
                    "Glob" => "glob",
                    "Grep" => "grep",
                    "Skill" => "load_skill",
                    "Agent" | "Task" => "task",
                    "WebFetch" => "web_fetch",
                    "WebSearch" => "web_search",
                    _ => return Err(format!("unsupported Claude tool mapping: {name}")),
                }
                .to_owned())
            })
            .collect()
    }
}

/// Convert a complete standalone document to validated native JSON. Imports
/// never install or execute the source. Unsupported fields fail with a diagnostic.
///
/// # Errors
/// Malformed, oversized or semantically unsupported input.
pub fn import_agent(text: &str, format: AgentImportFormat) -> Result<String, String> {
    if text.len() > MAX_TEXT_BYTES {
        return Err("agent import exceeds 1 MiB".to_owned());
    }
    let document = match format {
        AgentImportFormat::Codex => {
            let value: toml::Value = toml::from_str(text).map_err(|_| "invalid Codex TOML")?;
            let table = value.as_table().ok_or("Codex agent must be a TOML table")?;
            reject_fields(
                table.keys().map(String::as_str),
                &[
                    "name",
                    "description",
                    "developer_instructions",
                    "model",
                    "model_reasoning_effort",
                    "sandbox_mode",
                ],
            )?;
            let source: CodexAgent = toml::from_str(text).map_err(|_| "invalid Codex TOML or unsupported field (supported: name, description, developer_instructions, model, model_reasoning_effort, sandbox_mode)".to_owned())?;
            if source.name.trim().is_empty() || source.description.trim().is_empty() {
                return Err("name and description are required".to_owned());
            }
            let permissions = match source.sandbox_mode.as_deref() {
                None => ChildPermissions::Inherit,
                Some("read-only") => ChildPermissions::ReadOnly,
                Some(_) => return Err("unsupported sandbox_mode: only read-only has an equivalent native permission ceiling".to_owned()),
            };
            AgentDocument {
                display: source.name,
                description: Some(source.description),
                instructions: source.developer_instructions,
                provider: None,
                mode: AgentModeDocument::OneShot,
                config: SubagentConfig {
                    model: source.model,
                    effort: source.model_reasoning_effort,
                    permissions,
                    ..Default::default()
                },
            }
        }
        AgentImportFormat::Claude => {
            let text = text.replace("\r\n", "\n");
            let front = text
                .strip_prefix("---\n")
                .ok_or("Claude Markdown needs YAML front matter")?;
            let (yaml, body) = front
                .split_once("\n---\n")
                .ok_or("Claude YAML front matter is not closed")?;
            let value: serde_json::Value =
                serde_saphyr::from_str(yaml).map_err(|_| "invalid Claude YAML")?;
            let table = value
                .as_object()
                .ok_or("Claude front matter must be a mapping")?;
            reject_fields(
                table.keys().map(String::as_str),
                &[
                    "name",
                    "description",
                    "model",
                    "effort",
                    "tools",
                    "disallowedTools",
                    "permissionMode",
                    "maxTurns",
                    "memory",
                    "isolation",
                    "background",
                    "mcpServers",
                ],
            )?;
            let source: ClaudeAgent = serde_saphyr::from_str(yaml).map_err(|_| "invalid Claude YAML or unsupported field (hooks, skills preload, inline MCP definitions and initialPrompt have no equivalent import)".to_owned())?;
            if source.name.trim().is_empty() || source.description.trim().is_empty() {
                return Err("name and description are required".to_owned());
            }
            let permissions = match source.permissions.as_deref() {
                None => ChildPermissions::Inherit,
                Some("plan") => ChildPermissions::ReadOnly,
                Some("default" | "manual") => ChildPermissions::Default,
                Some(_) => return Err("unsupported permissionMode: supported values are plan, default and manual; omit to inherit".to_owned()),
            };
            let model = match source.model.as_deref() {
                None | Some("inherit") => None,
                Some("sonnet" | "opus" | "haiku" | "fable") => return Err("Claude model aliases require an explicit model id for the configured native inference provider".to_owned()),
                Some(_) => source.model,
            };
            AgentDocument {
                display: source.name,
                description: Some(source.description),
                instructions: body.trim().to_owned(),
                provider: None,
                mode: AgentModeDocument::OneShot,
                config: SubagentConfig {
                    model,
                    mcp_servers: source.mcp_servers,
                    effort: source.effort,
                    permissions,
                    tools: source.tools.map(Names::tools).transpose()?,
                    denied_tools: source
                        .denied_tools
                        .map(Names::tools)
                        .transpose()?
                        .unwrap_or_default(),
                    max_turns: source.max_turns,
                    memory: source.memory.unwrap_or_default(),
                    isolation: source.isolation.unwrap_or_default(),
                    background: source.background.unwrap_or(false),
                    ..Default::default()
                },
            }
        }
    };
    let json =
        serde_json::to_string_pretty(&document).map_err(|_| "cannot encode imported agent")?;
    super::user_declarations::validate_agent_document(&json)?;
    Ok(json)
}

fn reject_fields<'a>(
    fields: impl Iterator<Item = &'a str>,
    supported: &[&str],
) -> Result<(), String> {
    let unknown: Vec<_> = fields
        .filter(|field| !supported.contains(field))
        .map(|field| field.escape_debug().to_string())
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!("unsupported import fields: {}", unknown.join(", ")))
    }
}
