//! Strict, documented source subsets. Unknown semantics become visible omissions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use heycode_config::imports::{MAX_FILE_BYTES, MAX_RESOURCES, MAX_TOTAL_BYTES};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::*;

pub(super) fn discover(
    request: &ConfigImportRequest,
) -> Result<ConfigImportInventory, ImportError> {
    let source = ImportSource::open(&request.source_root)?;
    if let Some(project) = request.target.project_root() {
        request.target.recheck()?;
        if source.canonical_root() != project {
            return Err(ImportError::Authority);
        }
    }
    let project = matches!(request.target, ImportTarget::Project { .. });
    let directory = match request.product {
        ImportProduct::Codex => ".codex",
        ImportProduct::Gemini => ".gemini",
        ImportProduct::Cursor => ".cursor",
    };
    let mut scan = Scan {
        source,
        product: request.product,
        inputs: InventoryInputs::default(),
        rows: Vec::new(),
        candidates: Vec::new(),
        total: 0,
        skills_binding: false,
        instructions_binding: false,
        project,
    };
    let prefix = Path::new(directory);
    match request.product {
        ImportProduct::Codex => {
            scan.config(&prefix.join("config.toml"))?;
            scan.codex_agents(&prefix.join("agents"))?;
            scan.skills(Path::new(".agents/skills"))?;
            let instruction_root = if project { Path::new("") } else { prefix };
            let override_path = instruction_root.join("AGENTS.override.md");
            let normal_path = instruction_root.join("AGENTS.md");
            let override_file = scan.read(&override_path)?;
            if let Some(text) = override_file.filter(|text| !text.trim().is_empty()) {
                scan.instructions(&override_path, "agents", &text)?;
            } else if let Some(text) = scan
                .read(&normal_path)?
                .filter(|text| !text.trim().is_empty())
            {
                scan.instructions(&normal_path, "agents", &text)?;
            }
        }
        ImportProduct::Gemini => {
            scan.config(&prefix.join("settings.json"))?;
            scan.commands(&prefix.join("commands"), &prefix.join("commands"), 0)?;
            scan.skills(&prefix.join("skills"))?;
            let instruction_path = if project {
                PathBuf::from("GEMINI.md")
            } else {
                prefix.join("GEMINI.md")
            };
            if let Some(text) = scan.read(&instruction_path)? {
                scan.instructions(&instruction_path, "gemini", &text)?;
            }
        }
        ImportProduct::Cursor => {
            scan.config(&prefix.join("mcp.json"))?;
            if project {
                scan.cursor_rules(&prefix.join("rules"))?;
            }
        }
    }
    Ok(ConfigImportInventory {
        id: uuid::Uuid::new_v4().to_string(),
        target: request.target.clone(),
        rows: scan.rows,
        candidates: scan.candidates,
        inputs: Arc::new(scan.inputs),
    })
}

struct Scan {
    source: ImportSource,
    product: ImportProduct,
    inputs: InventoryInputs,
    rows: Vec<ConfigImportItem>,
    candidates: Vec<Option<Candidate>>,
    total: usize,
    skills_binding: bool,
    instructions_binding: bool,
    project: bool,
}

impl Scan {
    fn read(&mut self, path: &Path) -> Result<Option<String>, ImportError> {
        if self.inputs.files.len() >= MAX_RESOURCES {
            return Err(ImportError::Limit);
        }
        let file = self.source.read(path)?;
        self.total = self
            .total
            .checked_add(file.as_ref().map_or(0, ImportSourceFile::len))
            .ok_or(ImportError::Limit)?;
        if self.total > MAX_TOTAL_BYTES {
            return Err(ImportError::Limit);
        }
        let text = file
            .as_ref()
            .map(ImportSourceFile::text_for_parser)
            .transpose()?
            .map(str::to_owned);
        self.inputs
            .files
            .push((self.source.clone(), path.to_owned(), file));
        Ok(text)
    }

    fn entries(&mut self, path: &Path) -> Result<Vec<PathBuf>, ImportError> {
        if self.inputs.directories.len() >= MAX_RESOURCES {
            return Err(ImportError::Limit);
        }
        let names = self.source.entries(path)?;
        self.inputs
            .directories
            .push((self.source.clone(), path.to_owned(), names.clone()));
        Ok(names)
    }

    fn row(
        &mut self,
        label: &Path,
        candidate: Option<Candidate>,
        kind: Option<ImportResourceKind>,
        status: ImportItemStatus,
        reason: ImportItemReason,
    ) -> Result<(), ImportError> {
        if self.rows.len() >= MAX_RESOURCES {
            return Err(ImportError::Limit);
        }
        let suggested_name = candidate
            .as_ref()
            .map(|row| row.name.clone())
            .filter(|name| valid_name(name) && !suspected_secret(name));
        let (status, reason) = if candidate.is_some() && suggested_name.is_none() {
            (
                ImportItemStatus::NeedsRename,
                ImportItemReason::InvalidDestinationName,
            )
        } else {
            (status, reason)
        };
        self.rows.push(ConfigImportItem {
            id: format!("item-{}", self.rows.len() + 1),
            kind,
            label: safe_label(label),
            suggested_name,
            status,
            reason,
        });
        self.candidates.push(candidate);
        Ok(())
    }

    fn blocked(
        &mut self,
        label: &Path,
        kind: Option<ImportResourceKind>,
        status: ImportItemStatus,
        reason: ImportItemReason,
    ) -> Result<(), ImportError> {
        self.row(label, None, kind, status, reason)
    }

    fn ready(
        &mut self,
        label: &Path,
        kind: ImportResourceKind,
        name: String,
        payload: String,
        reason: ImportItemReason,
    ) -> Result<(), ImportError> {
        self.row(
            label,
            Some(Candidate {
                product: self.product,
                kind,
                name,
                payload,
            }),
            Some(kind),
            ImportItemStatus::Ready,
            reason,
        )
    }

    fn config(&mut self, path: &Path) -> Result<(), ImportError> {
        let Some(text) = self.read(path)? else {
            return Ok(());
        };
        let document = if self.product == ImportProduct::Codex {
            toml::from_str::<toml::Value>(&text)
                .ok()
                .and_then(|value| serde_json::to_value(value).ok())
        } else {
            serde_json::from_str::<Value>(&text).ok()
        };
        let Some(Value::Object(root)) = document else {
            return self.blocked(
                path,
                None,
                ImportItemStatus::Unsupported,
                ImportItemReason::InvalidDocument,
            );
        };
        if !bounded_value(&Value::Object(root.clone()), 0, &mut 0) {
            return Err(ImportError::Limit);
        }
        let mcp_key = if self.product == ImportProduct::Codex {
            "mcp_servers"
        } else {
            "mcpServers"
        };
        for (key, value) in &root {
            if key == "skills" {
                self.skills_binding = true;
            }
            if matches!(
                key.as_str(),
                "project_doc_fallback_filenames"
                    | "project_doc_max_bytes"
                    | "instructions"
                    | "developer_instructions"
                    | "context"
            ) {
                self.instructions_binding = true;
            }
            if key == mcp_key {
                let Some(servers) = value.as_object() else {
                    self.blocked(
                        &path.join("mcp-servers"),
                        Some(ImportResourceKind::Mcp),
                        ImportItemStatus::Unsupported,
                        ImportItemReason::InvalidDocument,
                    )?;
                    continue;
                };
                for (name, server) in servers {
                    self.mcp(&path.join("mcp-servers").join(name), name, server)?;
                }
            } else {
                let (status, reason) = if credential_key(key) {
                    (
                        ImportItemStatus::Excluded,
                        ImportItemReason::CredentialOrInterpolation,
                    )
                } else if matches!(
                    key.as_str(),
                    "model" | "model_provider" | "model_providers" | "model_reasoning_effort"
                ) {
                    (
                        ImportItemStatus::NeedsBinding,
                        ImportItemReason::ProviderBinding,
                    )
                } else if matches!(
                    key.as_str(),
                    "projects"
                        | "sandbox"
                        | "sandbox_mode"
                        | "approval_policy"
                        | "permissions"
                        | "hooks"
                        | "trust"
                        | "security"
                        | "tools"
                        | "extensions"
                ) {
                    (
                        ImportItemStatus::Excluded,
                        ImportItemReason::AuthorityExcluded,
                    )
                } else {
                    (
                        ImportItemStatus::Unsupported,
                        ImportItemReason::UnsupportedFields,
                    )
                };
                // Unknown keys can themselves contain secrets. Only allowlisted
                // well-known key names reach the metadata label.
                let known = matches!(
                    key.as_str(),
                    "model"
                        | "model_provider"
                        | "model_providers"
                        | "model_reasoning_effort"
                        | "projects"
                        | "sandbox"
                        | "sandbox_mode"
                        | "approval_policy"
                        | "permissions"
                        | "hooks"
                        | "trust"
                        | "security"
                        | "tools"
                        | "extensions"
                );
                self.blocked(
                    &path.join(if known { key } else { "unmapped-field" }),
                    None,
                    status,
                    reason,
                )?;
            }
        }
        Ok(())
    }

    fn mcp(&mut self, label: &Path, name: &str, value: &Value) -> Result<(), ImportError> {
        let kind = Some(ImportResourceKind::Mcp);
        // The native MCP management writer currently only owns the user layer.
        // Do not install project entries that a later edit could promote there.
        if self.project {
            return self.blocked(
                label,
                kind,
                ImportItemStatus::NeedsBinding,
                ImportItemReason::ScopeBinding,
            );
        }
        let Some(entry) = value.as_object() else {
            return self.blocked(
                label,
                kind,
                ImportItemStatus::Unsupported,
                ImportItemReason::InvalidDocument,
            );
        };
        if entry.keys().any(|key| credential_key(key)) || contains_interpolation(value) {
            return self.blocked(
                label,
                kind,
                ImportItemStatus::NeedsBinding,
                ImportItemReason::CredentialOrInterpolation,
            );
        }
        let allowed: &[&str] = match self.product {
            ImportProduct::Codex => &["command", "args", "url", "enabled"],
            ImportProduct::Gemini => &["command", "args", "httpUrl", "url"],
            ImportProduct::Cursor => &["command", "args", "url"],
        };
        if entry.keys().any(|key| !allowed.contains(&key.as_str())) {
            return self.blocked(
                label,
                kind,
                ImportItemStatus::Unsupported,
                ImportItemReason::UnsupportedFields,
            );
        }
        let has_command = entry.contains_key("command");
        let has_url = entry.contains_key("url");
        let has_http_url = entry.contains_key("httpUrl");
        if [has_command, has_url, has_http_url]
            .into_iter()
            .filter(|present| *present)
            .count()
            != 1
        {
            return self.blocked(
                label,
                kind,
                ImportItemStatus::NeedsBinding,
                ImportItemReason::TransportBinding,
            );
        }
        if has_url && self.product != ImportProduct::Codex {
            // Gemini url is legacy SSE; Cursor url does not identify HTTP vs SSE.
            return self.blocked(
                label,
                kind,
                ImportItemStatus::NeedsBinding,
                ImportItemReason::TransportBinding,
            );
        }
        if entry
            .get("enabled")
            .is_some_and(|value| !value.is_boolean())
        {
            return self.blocked(
                label,
                kind,
                ImportItemStatus::Unsupported,
                ImportItemReason::InvalidDocument,
            );
        }
        // After checking the actual source schema and selecting its exact
        // transport, reuse the existing credential-screening transport parser.
        let mut native = Map::new();
        if has_command {
            native.insert("command".to_owned(), entry["command"].clone());
            if let Some(args) = entry.get("args") {
                native.insert("args".to_owned(), args.clone());
            }
        } else {
            if entry.contains_key("args") {
                return self.blocked(
                    label,
                    kind,
                    ImportItemStatus::Unsupported,
                    ImportItemReason::UnsupportedFields,
                );
            }
            native.insert("type".to_owned(), Value::String("http".to_owned()));
            native.insert(
                "url".to_owned(),
                entry[if has_http_url { "httpUrl" } else { "url" }].clone(),
            );
        }
        let raw = serde_json::json!({"mcpServers": {"candidate": native}}).to_string();
        let preview = match heycode_config::preview_competitor_config(
            heycode_config::CompetitorConfigKind::ClaudeMcp,
            heycode_config::ImportAuthority::user(true),
            &raw,
        ) {
            Ok(preview) => preview,
            Err(_) => {
                return self.blocked(
                    label,
                    kind,
                    ImportItemStatus::Unsupported,
                    ImportItemReason::InvalidDocument,
                );
            }
        };
        let Some(server) = preview.mcp_servers().first() else {
            return self.blocked(
                label,
                kind,
                ImportItemStatus::Excluded,
                ImportItemReason::CredentialOrInterpolation,
            );
        };
        let payload = match server.transport() {
            heycode_config::ImportedMcpTransport::Stdio { command, args } => {
                ImportedMcpDocument::Stdio {
                    command: command.clone(),
                    args: args.clone(),
                }
            }
            heycode_config::ImportedMcpTransport::StreamableHttp { url } => {
                ImportedMcpDocument::StreamableHttp { url: url.clone() }
            }
        };
        self.ready(
            label,
            ImportResourceKind::Mcp,
            name.to_owned(),
            serialize_private(&payload)?,
            ImportItemReason::McpInstalledDisabled,
        )
    }

    fn codex_agents(&mut self, directory: &Path) -> Result<(), ImportError> {
        for path in self.entries(directory)? {
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let Some(text) = self.read(&path)? else {
                return Err(ImportError::Stale);
            };
            let Some(table) = toml::from_str::<toml::Table>(&text).ok() else {
                self.blocked(
                    &path,
                    Some(ImportResourceKind::Agent),
                    ImportItemStatus::Unsupported,
                    ImportItemReason::InvalidDocument,
                )?;
                continue;
            };
            if table.contains_key("model") || table.contains_key("model_reasoning_effort") {
                self.blocked(
                    &path,
                    Some(ImportResourceKind::Agent),
                    ImportItemStatus::NeedsBinding,
                    ImportItemReason::ProviderBinding,
                )?;
                continue;
            }
            if suspected_secret(&text) {
                self.blocked(
                    &path,
                    Some(ImportResourceKind::Agent),
                    ImportItemStatus::Excluded,
                    ImportItemReason::CredentialOrInterpolation,
                )?;
                continue;
            }
            match crate::import_agent(&text, crate::AgentImportFormat::Codex) {
                Ok(document) => {
                    let name = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("")
                        .to_owned();
                    self.ready(
                        &path,
                        ImportResourceKind::Agent,
                        name,
                        document,
                        ImportItemReason::Supported,
                    )?;
                }
                Err(_) => self.blocked(
                    &path,
                    Some(ImportResourceKind::Agent),
                    ImportItemStatus::Unsupported,
                    ImportItemReason::UnsupportedFields,
                )?,
            }
        }
        Ok(())
    }

    fn skills(&mut self, directory: &Path) -> Result<(), ImportError> {
        for path in self.entries(directory)? {
            // The fixed source reader rejects symlink/non-directory entries.
            let files = match self.entries(&path) {
                Ok(files) => files,
                Err(ImportError::UnsafePath) => {
                    self.blocked(
                        &path,
                        Some(ImportResourceKind::Skill),
                        ImportItemStatus::Unsupported,
                        ImportItemReason::ExternalDependency,
                    )?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let skill_file = path.join("SKILL.md");
            if !files.contains(&skill_file) {
                continue;
            }
            let Some(text) = self.read(&skill_file)? else {
                return Err(ImportError::Stale);
            };
            if self.product == ImportProduct::Gemini || self.skills_binding {
                self.blocked(
                    &skill_file,
                    Some(ImportResourceKind::Skill),
                    ImportItemStatus::NeedsBinding,
                    ImportItemReason::ActivationSemantics,
                )?;
                continue;
            }
            if files.len() != 1 {
                self.blocked(
                    &skill_file,
                    Some(ImportResourceKind::Skill),
                    ImportItemStatus::NeedsBinding,
                    ImportItemReason::ExternalDependency,
                )?;
                continue;
            }
            let parsed = parse_skill(&text);
            match parsed {
                Ok((name, skill)) => self.ready(
                    &skill_file,
                    ImportResourceKind::Skill,
                    name,
                    serialize_private(&skill)?,
                    ImportItemReason::Supported,
                )?,
                Err(reason) => self.blocked(
                    &skill_file,
                    Some(ImportResourceKind::Skill),
                    ImportItemStatus::Unsupported,
                    reason,
                )?,
            }
        }
        Ok(())
    }

    fn commands(&mut self, directory: &Path, base: &Path, depth: usize) -> Result<(), ImportError> {
        if depth > 8 {
            return Err(ImportError::Limit);
        }
        for path in self.entries(directory)? {
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                if path.extension().is_none() {
                    match self.commands(&path, base, depth + 1) {
                        Ok(()) => {}
                        Err(ImportError::UnsafePath) => self.blocked(
                            &path,
                            Some(ImportResourceKind::Command),
                            ImportItemStatus::Unsupported,
                            ImportItemReason::ExternalDependency,
                        )?,
                        Err(error) => return Err(error),
                    }
                }
                continue;
            }
            let Some(text) = self.read(&path)? else {
                return Err(ImportError::Stale);
            };
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct GeminiCommand {
                prompt: String,
                description: Option<String>,
            }
            let source: GeminiCommand = match toml::from_str(&text) {
                Ok(source) => source,
                Err(_) => {
                    self.blocked(
                        &path,
                        Some(ImportResourceKind::Command),
                        ImportItemStatus::Unsupported,
                        ImportItemReason::UnsupportedFields,
                    )?;
                    continue;
                }
            };
            if source.prompt.contains("!{") || source.prompt.contains("@{") {
                self.blocked(
                    &path,
                    Some(ImportResourceKind::Command),
                    ImportItemStatus::Unsupported,
                    ImportItemReason::ExternalDependency,
                )?;
                continue;
            }
            if !valid_text(&source.prompt, MAX_FILE_BYTES) || suspected_secret(&source.prompt) {
                self.blocked(
                    &path,
                    Some(ImportResourceKind::Command),
                    ImportItemStatus::Excluded,
                    ImportItemReason::CredentialOrInterpolation,
                )?;
                continue;
            }
            let description = source
                .description
                .unwrap_or_else(|| "Imported Gemini prompt command".to_owned());
            if !safe_description(&description) {
                self.blocked(
                    &path,
                    Some(ImportResourceKind::Command),
                    ImportItemStatus::Excluded,
                    ImportItemReason::CredentialOrInterpolation,
                )?;
                continue;
            }
            let relative = path
                .strip_prefix(base)
                .map_err(|_| ImportError::UnsafePath)?
                .with_extension("");
            let name = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join(":");
            let document = ImportedCommandDocument {
                description,
                prompt: source.prompt,
                argument_mode: ImportedArgumentMode::Gemini,
            };
            self.ready(
                &path,
                ImportResourceKind::Command,
                name,
                serialize_private(&document)?,
                ImportItemReason::Supported,
            )?;
        }
        Ok(())
    }

    fn instructions(&mut self, path: &Path, name: &str, text: &str) -> Result<(), ImportError> {
        if self.instructions_binding {
            return self.blocked(
                path,
                Some(ImportResourceKind::Instructions),
                ImportItemStatus::NeedsBinding,
                ImportItemReason::ActivationSemantics,
            );
        }
        if !valid_text(text, heycode_prompt::instructions::MAX_INSTRUCTION_BYTES) {
            return self.blocked(
                path,
                Some(ImportResourceKind::Instructions),
                ImportItemStatus::Unsupported,
                ImportItemReason::InvalidDocument,
            );
        }
        if suspected_secret(text) {
            return self.blocked(
                path,
                Some(ImportResourceKind::Instructions),
                ImportItemStatus::Excluded,
                ImportItemReason::CredentialOrInterpolation,
            );
        }
        if self.product == ImportProduct::Gemini && text.contains('@') {
            return self.blocked(
                path,
                Some(ImportResourceKind::Instructions),
                ImportItemStatus::Unsupported,
                ImportItemReason::ExternalDependency,
            );
        }
        self.ready(
            path,
            ImportResourceKind::Instructions,
            name.to_owned(),
            serialize_private(&ImportedInstructionDocument {
                text: text.to_owned(),
            })?,
            ImportItemReason::Supported,
        )
    }

    fn cursor_rules(&mut self, directory: &Path) -> Result<(), ImportError> {
        for path in self.entries(directory)? {
            if path.extension().and_then(|value| value.to_str()) != Some("mdc") {
                continue;
            }
            let Some(text) = self.read(&path)? else {
                return Err(ImportError::Stale);
            };
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Rule {
                #[serde(rename = "alwaysApply")]
                always_apply: Option<bool>,
                #[serde(rename = "description")]
                _description: Option<String>,
                #[serde(rename = "globs")]
                _globs: Option<Value>,
            }
            let rule = frontmatter(&text).and_then(|(yaml, body)| {
                serde_saphyr::from_str::<Rule>(yaml)
                    .map(|rule| (rule, body))
                    .map_err(|_| ImportItemReason::InvalidDocument)
            });
            match rule {
                Ok((rule, body)) if rule.always_apply == Some(true) && !body.contains('@') => {
                    let name = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("");
                    self.instructions(&path, name, body)?;
                }
                Ok(_) => self.blocked(
                    &path,
                    Some(ImportResourceKind::Instructions),
                    ImportItemStatus::Unsupported,
                    ImportItemReason::ActivationSemantics,
                )?,
                Err(reason) => self.blocked(
                    &path,
                    Some(ImportResourceKind::Instructions),
                    ImportItemStatus::Unsupported,
                    reason,
                )?,
            }
        }
        Ok(())
    }
}

fn frontmatter(text: &str) -> Result<(&str, &str), ImportItemReason> {
    text.strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .ok_or(ImportItemReason::InvalidDocument)
}

fn parse_skill(text: &str) -> Result<(String, ImportedSkillDocument), ImportItemReason> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Skill {
        name: String,
        description: String,
        #[serde(default, rename = "disable-model-invocation")]
        disable: bool,
    }
    let normalized = text.replace("\r\n", "\n");
    let (yaml, body) = frontmatter(&normalized)?;
    let source: Skill =
        serde_saphyr::from_str(yaml).map_err(|_| ImportItemReason::UnsupportedFields)?;
    if !safe_description(&source.description)
        || !valid_text(body, MAX_FILE_BYTES)
        || suspected_secret(body)
    {
        return Err(ImportItemReason::CredentialOrInterpolation);
    }
    Ok((
        source.name,
        ImportedSkillDocument {
            description: source.description,
            disable_model_invocation: source.disable,
            body: body.to_owned(),
        },
    ))
}

pub(super) fn valid_text(text: &str, maximum: usize) -> bool {
    !text.trim().is_empty()
        && text.len() <= maximum
        && !text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
}

pub(super) fn safe_description(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= 256
        && !text.chars().any(char::is_control)
        && !suspected_secret(text)
}

pub(super) fn suspected_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "sk_live_",
        "sk_test_",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "-----begin private key",
        "-----begin rsa private key",
        "bearer ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || lower
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
            .any(|word| word.starts_with("sk-") && word.len() >= 16)
        || text
            .split_whitespace()
            .any(|part| part.starts_with("eyJ") && part.split('.').count() == 3)
}

fn credential_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "auth",
        "credential",
        "secret",
        "password",
        "token",
        "api_key",
        "apikey",
        "header",
        "env",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn contains_interpolation(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains('$') || suspected_secret(text),
        Value::Array(values) => values.iter().any(contains_interpolation),
        Value::Object(values) => values.values().any(contains_interpolation),
        _ => false,
    }
}

fn bounded_value(value: &Value, depth: usize, fields: &mut usize) -> bool {
    *fields += 1;
    if depth > 32 || *fields > 2048 {
        return false;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .all(|value| bounded_value(value, depth + 1, fields)),
        Value::Object(values) => values
            .values()
            .all(|value| bounded_value(value, depth + 1, fields)),
        _ => true,
    }
}
