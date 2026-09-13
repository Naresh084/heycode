//! Hooks and subagent presets a user writes as plain files.
//!
//! Plugins contribute hooks and presets as declarative documents. A user
//! should not need to author, install and enable a plugin to add one hook or
//! one agent, so the same documents are also read from files:
//!
//! - `$HEYCODE_HOME/hooks/*.json` and `$HEYCODE_HOME/agents/*.json` — the user's own,
//!   always loaded.
//! - `<workspace>/.heycode/hooks/*.json` and `<workspace>/.heycode/agents/*.json` —
//!   the project's, loaded under the same trust gate as every other project
//!   input. A project hook whose action runs a command is executable
//!   authority and needs the executable gate, not just the instructions one.
//!
//! `/agent-config` authors, validates, imports and reloads these declarations.
//! Invalid files are diagnostic rows, never a refusal to start.

use std::path::{Path, PathBuf};

use heycode_agent::{SubagentPreset, SubagentProviderId};
use heycode_hooks::{Hook, HookAction};
use serde::Deserialize;

use crate::{AgentDocument, HookActionDocument, HookDocument, MAX_TEXT_BYTES};

/// Where user-authored declaration files may be read from.
#[derive(Debug, Clone, Default)]
pub struct UserDeclarationRoots {
    /// The user's heycode home.
    pub user_home: Option<PathBuf>,
    /// The trusted workspace root, and whether project *executable* authority
    /// (hooks that run commands) is granted for it.
    pub workspace: Option<WorkspaceDeclarationAccess>,
}

/// What a project may contribute from its `.heycode/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDeclarationAccess {
    /// Workspace root.
    pub root: PathBuf,
    /// Whether project hooks may run commands.
    pub executables_allowed: bool,
}

/// One file that was not loaded, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedDeclaration {
    /// Display path relative to its root.
    pub path: String,
    /// Validated diagnostic, including a field name where available.
    pub reason: String,
}

/// Everything the files yielded.
#[derive(Default)]
pub struct UserDeclarations {
    /// Hooks in file order, user home first.
    pub hooks: Vec<Hook>,
    /// Presets in file order, user home first.
    pub presets: Vec<SubagentPreset>,
    /// Files that were skipped.
    pub skipped: Vec<SkippedDeclaration>,
}

/// Read every declaration file the roots allow.
#[must_use]
pub fn load_user_declarations(roots: &UserDeclarationRoots) -> UserDeclarations {
    let mut out = UserDeclarations::default();
    if let Some(home) = &roots.user_home {
        load_root(&mut out, home, "user", false, true);
    }
    if let Some(workspace) = &roots.workspace {
        load_root(
            &mut out,
            &workspace.root.join(".heycode"),
            "project",
            true,
            workspace.executables_allowed,
        );
    }
    // Scope-qualified ids remain available. Bare aliases select project over
    // user, independent of filesystem enumeration order.
    let mut effective = std::collections::BTreeMap::new();
    for preset in &out.presets {
        if let Some((_, name)) = preset.id().as_str().split_once('-')
            && !name.starts_with("user-")
            && !name.starts_with("project-")
            && let Ok(alias) = preset.clone().with_id(name)
        {
            effective.insert(name.to_owned(), alias);
        }
    }
    out.presets.extend(effective.into_values());
    out
}

/// Merge already-validated, composition-pinned imported presets below native
/// declarations in the same scope. Bare aliases still prefer project scope.
/// The returned generation is reused by `/agent-config reload` without reading
/// a newer import-store pointer.
pub fn load_user_declarations_with_imports(
    roots: &UserDeclarationRoots,
    imported: &[SubagentPreset],
) -> UserDeclarations {
    let mut loaded = load_user_declarations(roots);
    loaded.presets.retain(|preset| {
        preset.id().as_str().starts_with("user-") || preset.id().as_str().starts_with("project-")
    });
    for preset in imported {
        let id = preset.id().as_str();
        if !(id.starts_with("user-") || (id.starts_with("project-") && roots.workspace.is_some())) {
            loaded.skipped.push(SkippedDeclaration {
                path: "imports/agents".to_owned(),
                reason: "imported agent scope is not admitted".to_owned(),
            });
            continue;
        }
        if loaded
            .presets
            .iter()
            .any(|native| native.id() == preset.id())
        {
            loaded.skipped.push(SkippedDeclaration {
                path: format!("imports/agents/{id}"),
                reason: "native declaration takes precedence in this scope".to_owned(),
            });
        } else {
            loaded.presets.push(preset.clone());
        }
    }
    loaded.presets.sort_by_key(|preset| {
        (
            preset.id().as_str().starts_with("project-"),
            preset.id().as_str().to_owned(),
        )
    });
    let mut aliases = std::collections::BTreeMap::new();
    for preset in &loaded.presets {
        if let Some((_, name)) = preset.id().as_str().split_once('-')
            && !name.starts_with("user-")
            && !name.starts_with("project-")
            && let Ok(alias) = preset.clone().with_id(name)
        {
            aliases.insert(name.to_owned(), alias);
        }
    }
    loaded.presets.extend(aliases.into_values());
    loaded
}

/// Validate a native custom-agent JSON document without publishing it.
///
/// # Errors
/// Malformed, unknown or invalid fields.
pub fn validate_agent_document(text: &str) -> Result<(), String> {
    preset_from_file(text, "user", "validation").map(|_| ())
}

fn load_root(
    out: &mut UserDeclarations,
    root: &Path,
    scope: &str,
    project_scoped: bool,
    executables_allowed: bool,
) {
    if project_scoped
        && std::fs::symlink_metadata(root).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        out.skipped.push(SkippedDeclaration {
            path: "agents/".to_owned(),
            reason: "project .heycode directory must not be a symlink".to_owned(),
        });
        return;
    }
    for (file, stem, text) in json_files(&root.join("hooks"), "hooks", &mut out.skipped) {
        match hook_from_file(&text, scope, &stem, project_scoped, executables_allowed) {
            Ok(hook) => out.hooks.push(hook),
            Err(reason) => out.skipped.push(SkippedDeclaration {
                path: format!("hooks/{file}"),
                reason: reason.to_owned(),
            }),
        }
    }
    for (file, stem, text) in json_files(&root.join("agents"), "agents", &mut out.skipped) {
        match preset_from_file(&text, scope, &stem) {
            Ok(preset) => out.presets.push(preset),
            Err(reason) => out.skipped.push(SkippedDeclaration {
                path: format!("agents/{file}"),
                reason: reason.to_owned(),
            }),
        }
    }
}

/// `(file name, stem, contents)` for every regular `*.json` file, sorted.
fn json_files(
    directory: &Path,
    kind: &str,
    skipped: &mut Vec<SkippedDeclaration>,
) -> Vec<(String, String, String)> {
    if std::fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        skipped.push(SkippedDeclaration {
            path: format!("{kind}/"),
            reason: "declaration directory must not be a symlink".to_owned(),
        });
        return Vec::new();
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => {
            skipped.push(SkippedDeclaration {
                path: format!("{kind}/"),
                reason: "cannot read declarations directory".to_owned(),
            });
            return Vec::new();
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(_) => skipped.push(SkippedDeclaration {
                path: format!("{kind}/"),
                reason: "cannot enumerate declaration file".to_owned(),
            }),
        }
    }
    paths.sort();
    let mut files = Vec::new();
    for path in paths {
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
            skipped.push(SkippedDeclaration {
                path: format!("{kind}/<non-UTF-8>"),
                reason: "file name must be UTF-8".to_owned(),
            });
            continue;
        };
        let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        match crate::agent_management::read_bounded(&path) {
            Ok(text) => files.push((file.to_owned(), stem.to_owned(), text)),
            Err(_) => skipped.push(SkippedDeclaration {
                path: format!("{kind}/{file}"),
                reason: "file must be readable UTF-8, regular, not a symlink, and at most 1 MiB"
                    .to_owned(),
            }),
        }
    }
    files
}

fn parse<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, &'static str> {
    if text.len() > MAX_TEXT_BYTES {
        return Err("document is larger than 1 MiB");
    }
    serde_json::from_str(text).map_err(|_| "document is not the expected JSON shape")
}

fn hook_from_file(
    text: &str,
    scope: &str,
    stem: &str,
    project_scoped: bool,
    executables_allowed: bool,
) -> Result<Hook, &'static str> {
    let document: HookDocument = parse(text)?;
    let action = match document.action {
        HookActionDocument::Command { command } => {
            if project_scoped && !executables_allowed {
                return Err("a project hook that runs a command needs workspace trust");
            }
            bounded(&command)?;
            HookAction::Command(command)
        }
        HookActionDocument::Prompt { prompt } => {
            bounded(&prompt)?;
            HookAction::Prompt(prompt)
        }
        HookActionDocument::Subagent { agent, prompt } => {
            bounded(&agent)?;
            bounded(&prompt)?;
            HookAction::Subagent { agent, prompt }
        }
        HookActionDocument::McpTool {
            server,
            tool,
            arguments,
        } => {
            bounded(&server)?;
            bounded(&tool)?;
            HookAction::McpTool {
                server,
                tool,
                arguments,
            }
        }
    };
    Ok(Hook {
        owner: format!("{scope}:{stem}"),
        phase: document.phase.resolve(),
        event: document.event.resolve(),
        action,
        project_scoped,
    })
}

pub(crate) fn preset_from_file(
    text: &str,
    scope: &str,
    stem: &str,
) -> Result<SubagentPreset, String> {
    if text.len() > MAX_TEXT_BYTES {
        return Err("agent document exceeds 1 MiB".to_owned());
    }
    let document: AgentDocument =
        serde_json::from_str(text).map_err(|error| format!("invalid agent JSON: {error}"))?;
    bounded(&document.display)?;
    if document.display.len() > 256
        || document.display.trim() != document.display
        || document.display.chars().any(char::is_control)
    {
        return Err("display must be at most 256 bytes without surrounding whitespace or control characters".to_owned());
    }
    bounded(&document.instructions)?;
    let provider = document
        .provider
        .map(SubagentProviderId::new)
        .transpose()
        .map_err(|_| "provider is not a valid subagent provider id")?;
    let (seed, continuation) = document.mode.resolve();
    // The file stem is the preset id, so `/agents` and the `task` tool name
    // the file the user wrote.
    SubagentPreset::new(
        format!("{scope}-{stem}"),
        document.display,
        document.instructions,
        provider,
        seed,
        continuation,
    )
    .map_err(|_| "file name must be kebab-case (it becomes the preset id)")?
    .with_description(document.description)
    .map_err(|_| "agent description is invalid")?
    .with_config(document.config)
    .map_err(|error| error.to_string())
}

fn bounded(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("a text field is empty, over 1 MiB or carries control characters");
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn user_and_trusted_project_files_load_and_bad_ones_are_skipped_rows() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        write(
            home.path(),
            "hooks/lint.json",
            r#"{"phase":"post","event":"tool_use","action":{"type":"command","command":"cargo fmt"}}"#,
        );
        write(
            home.path(),
            "agents/reviewer.json",
            r#"{"display":"Reviewer","instructions":"Review the diff.","mode":"oneshot"}"#,
        );
        write(workspace.path(), ".heycode/hooks/broken.json", "{not json");
        write(
            workspace.path(),
            ".heycode/hooks/announce.json",
            r#"{"phase":"pre","event":"turn","action":{"type":"prompt","prompt":"Say hi."}}"#,
        );
        write(
            workspace.path(),
            ".heycode/agents/Bad Name.json",
            r#"{"display":"x","instructions":"y"}"#,
        );
        let loaded = load_user_declarations(&UserDeclarationRoots {
            user_home: Some(home.path().to_path_buf()),
            workspace: Some(WorkspaceDeclarationAccess {
                root: workspace.path().to_path_buf(),
                executables_allowed: true,
            }),
        });
        let owners: Vec<&str> = loaded.hooks.iter().map(|h| h.owner.as_str()).collect();
        assert_eq!(owners, ["user:lint", "project:announce"]);
        assert!(loaded.hooks[1].project_scoped);
        assert_eq!(loaded.presets.len(), 2);
        assert_eq!(loaded.presets[1].id().as_str(), "reviewer");
        assert_eq!(loaded.presets[0].id().as_str(), "user-reviewer");
        let skipped: Vec<&str> = loaded.skipped.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(skipped, ["hooks/broken.json", "agents/Bad Name.json"]);
    }

    #[test]
    fn a_project_command_hook_needs_executable_trust() {
        let workspace = tempfile::tempdir().unwrap();
        write(
            workspace.path(),
            ".heycode/hooks/run.json",
            r#"{"phase":"post","event":"tool_use","action":{"type":"command","command":"rm -rf /"}}"#,
        );
        write(
            workspace.path(),
            ".heycode/hooks/say.json",
            r#"{"phase":"pre","event":"turn","action":{"type":"prompt","prompt":"Be careful."}}"#,
        );
        let read_only = load_user_declarations(&UserDeclarationRoots {
            user_home: None,
            workspace: Some(WorkspaceDeclarationAccess {
                root: workspace.path().to_path_buf(),
                executables_allowed: false,
            }),
        });
        assert_eq!(read_only.hooks.len(), 1, "the prompt hook loads");
        assert_eq!(read_only.skipped.len(), 1);
        assert!(read_only.skipped[0].reason.contains("workspace trust"));

        let trusted = load_user_declarations(&UserDeclarationRoots {
            user_home: None,
            workspace: Some(WorkspaceDeclarationAccess {
                root: workspace.path().to_path_buf(),
                executables_allowed: true,
            }),
        });
        assert_eq!(trusted.hooks.len(), 2);
        assert!(trusted.skipped.is_empty());

        assert!(
            load_user_declarations(&UserDeclarationRoots::default())
                .hooks
                .is_empty()
        );
    }
}
