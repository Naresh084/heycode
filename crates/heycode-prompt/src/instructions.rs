//! Project instructions: the `AGENTS.md` / `CLAUDE.md` files a user writes for
//! the agent, rendered into the system prompt.
//!
//! Codex reads `AGENTS.md`; Claude Code reads the `CLAUDE.md` hierarchy. heycode
//! reads both spellings plus `.heycode/INSTRUCTIONS.md`, from two places only: the
//! user's heycode home and the trusted workspace root. Nothing above the workspace
//! is read, because trust was granted for the workspace and not for its
//! parents.

use std::path::{Path, PathBuf};

/// Largest instruction file rendered in full; longer files are cut with a
/// visible marker so the model is told it did not see everything.
pub const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;

/// File names read from the workspace root, in render order.
pub const WORKSPACE_INSTRUCTION_FILES: [&str; 5] = [
    "AGENTS.md",
    "CLAUDE.md",
    ".heycode/INSTRUCTIONS.md",
    "AGENTS.local.md",
    "CLAUDE.local.md",
];

/// Where instruction files may be read from.
#[derive(Debug, Clone, Default)]
pub struct InstructionSources {
    /// The user's heycode home; `AGENTS.md` under it applies to every project.
    pub user_home: Option<PathBuf>,
    /// The trusted workspace root. `None` when workspace trust blocks
    /// project instructions.
    pub workspace: Option<PathBuf>,
}

/// One instruction file as loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    /// Display label: `~/.heycode/AGENTS.md` or the path relative to the workspace.
    pub label: String,
    /// File text, cut at [`MAX_INSTRUCTION_BYTES`] on a character boundary.
    pub text: String,
    /// Whether `text` was cut.
    pub truncated: bool,
}

/// Load every instruction file the sources allow, in render order.
///
/// Missing files are simply absent. Unreadable or non-UTF-8 files are skipped:
/// they are user data and must not stop the product.
#[must_use]
pub fn discover_instructions(sources: &InstructionSources) -> Vec<InstructionFile> {
    let scoped = discover_scoped_instructions(sources);
    scoped.user.into_iter().chain(scoped.project).collect()
}

/// Native guidance separated by its actual authority scope. Renderers must
/// retain this separation when interleaving imported guidance.
#[derive(Debug, Clone, Default)]
pub struct ScopedInstructionFiles {
    /// Native user guidance, before every project layer.
    pub user: Vec<InstructionFile>,
    /// Native project guidance, after imported guidance in that scope.
    pub project: Vec<InstructionFile>,
}

/// Discover the same bounded native files while retaining scope attribution.
#[must_use]
pub fn discover_scoped_instructions(sources: &InstructionSources) -> ScopedInstructionFiles {
    let mut files = ScopedInstructionFiles::default();
    if let Some(home) = &sources.user_home {
        push_file(
            &mut files.user,
            &home.join("AGENTS.md"),
            "~/.heycode/AGENTS.md",
        );
    }
    if let Some(workspace) = &sources.workspace {
        for name in WORKSPACE_INSTRUCTION_FILES {
            push_file(&mut files.project, &workspace.join(name), name);
        }
    }
    files
}

/// Exact named replacements used by native dynamic-workspace prompt rendering.
/// Empty layers are returned too, so stale captured guidance is withdrawn.
#[must_use]
pub fn render_scoped_instructions(sources: &InstructionSources) -> Vec<(&'static str, String)> {
    let files = discover_scoped_instructions(sources);
    vec![
        ("user-instructions", render_instructions(&files.user)),
        ("project-instructions", render_instructions(&files.project)),
    ]
}

fn push_file(files: &mut Vec<InstructionFile>, path: &Path, label: &str) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    // A symlink could point outside the trusted root; only regular files count.
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return;
    };
    let (text, truncated) = truncate_on_char_boundary(text, MAX_INSTRUCTION_BYTES);
    files.push(InstructionFile {
        label: label.to_owned(),
        text,
        truncated,
    });
}

fn truncate_on_char_boundary(text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut cut = limit;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_owned(), true)
}

/// Render loaded files as the `# Project instructions` prompt section, or an
/// empty string when there are none (the registry drops empty sections).
#[must_use]
pub fn render_instructions(files: &[InstructionFile]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "# Project instructions\n\nThe following files were written by the user or their team for you. Follow them.\n",
    );
    for file in files {
        out.push_str("\n## ");
        out.push_str(&file.label);
        out.push_str("\n\n");
        out.push_str(file.text.trim_end());
        if file.truncated {
            out.push_str(&format!(
                "\n\n… (truncated at {} KiB)",
                MAX_INSTRUCTION_BYTES / 1024
            ));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn workspace_files_render_in_order_after_the_user_home_file() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("AGENTS.md"), "home rules").unwrap();
        std::fs::write(workspace.path().join("CLAUDE.md"), "claude rules").unwrap();
        std::fs::write(workspace.path().join("AGENTS.md"), "agents rules").unwrap();
        std::fs::create_dir(workspace.path().join(".heycode")).unwrap();
        std::fs::write(
            workspace.path().join(".heycode/INSTRUCTIONS.md"),
            "heycode rules",
        )
        .unwrap();
        let files = discover_instructions(&InstructionSources {
            user_home: Some(home.path().to_path_buf()),
            workspace: Some(workspace.path().to_path_buf()),
        });
        let labels: Vec<&str> = files.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "~/.heycode/AGENTS.md",
                "AGENTS.md",
                "CLAUDE.md",
                ".heycode/INSTRUCTIONS.md"
            ]
        );
        let rendered = render_instructions(&files);
        assert!(rendered.starts_with("# Project instructions"));
        assert!(rendered.contains("## AGENTS.md\n\nagents rules"));
        assert!(rendered.find("home rules").unwrap() < rendered.find("agents rules").unwrap());
    }

    #[test]
    fn a_blocked_workspace_reads_only_the_user_home() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("AGENTS.md"), "home rules").unwrap();
        std::fs::write(workspace.path().join("AGENTS.md"), "MUST NOT LOAD").unwrap();
        let files = discover_instructions(&InstructionSources {
            user_home: Some(home.path().to_path_buf()),
            workspace: None,
        });
        assert_eq!(files.len(), 1);
        assert!(!render_instructions(&files).contains("MUST NOT LOAD"));
        assert_eq!(render_instructions(&[]), "");
    }

    #[test]
    fn oversized_files_are_cut_with_a_visible_marker_and_symlinks_are_ignored() {
        let workspace = tempfile::tempdir().unwrap();
        let big = "é".repeat(MAX_INSTRUCTION_BYTES);
        std::fs::write(workspace.path().join("AGENTS.md"), &big).unwrap();
        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(outside.path(), "OUTSIDE").unwrap();
            std::os::unix::fs::symlink(outside.path(), workspace.path().join("CLAUDE.md")).unwrap();
        }
        let files = discover_instructions(&InstructionSources {
            user_home: None,
            workspace: Some(workspace.path().to_path_buf()),
        });
        assert_eq!(files.len(), 1, "the symlinked CLAUDE.md is not read");
        assert!(files[0].truncated);
        assert!(files[0].text.len() <= MAX_INSTRUCTION_BYTES);
        assert!(render_instructions(&files).contains("truncated at 64 KiB"));
    }
}
