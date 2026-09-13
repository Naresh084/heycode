//! Prompts the user sent, remembered across runs.
//!
//! A shell remembers what you typed yesterday; before this, Up recalled only
//! what you had typed since the process started, so the first thing every new
//! session forgot was the command you were about to repeat.
//!
//! The file is one JSON string per line — a prompt is often multi-line, and a
//! quoted string is the only framing that cannot be confused by its own
//! content. Unreadable lines are skipped rather than discarding the file:
//! history is a convenience, and a convenience must never be a startup
//! failure.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Most prompts kept. Old entries fall off the front.
pub const MAX_PROMPT_HISTORY: usize = 500;

/// Owner-only prompt history file for one heycode home.
#[derive(Debug, Clone)]
pub struct PromptHistoryStore {
    path: PathBuf,
}

impl PromptHistoryStore {
    /// A store over one exact file.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// File this store reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Prompts oldest first. A missing, unreadable or partly corrupt file
    /// yields what could be read, never an error.
    #[must_use]
    pub fn load(&self) -> Vec<String> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut prompts: Vec<String> = raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<String>(line).ok())
            .collect();
        if prompts.len() > MAX_PROMPT_HISTORY {
            prompts.drain(..prompts.len() - MAX_PROMPT_HISTORY);
        }
        prompts
    }

    /// Append one prompt, trimming the file when it outgrows the cap.
    ///
    /// Best effort by design: a full disk or a read-only home costs the user
    /// their history, not their session.
    pub fn append(&self, prompt: &str) {
        if prompt.trim().is_empty() {
            return;
        }
        let Ok(line) = serde_json::to_string(prompt) else {
            return;
        };
        if let Some(parent) = self.path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let existing = self.load();
        if existing.len() + 1 > MAX_PROMPT_HISTORY {
            // Rewrite whole: cheap at this size, and it keeps the file from
            // growing without bound for a long-lived home.
            let mut kept = existing;
            kept.push(prompt.to_owned());
            kept.drain(..kept.len() - MAX_PROMPT_HISTORY);
            self.rewrite(&kept);
            return;
        }
        let Ok(mut file) = self.open_appending() else {
            return;
        };
        let _appended = writeln!(file, "{line}");
    }

    fn rewrite(&self, prompts: &[String]) {
        let body = prompts
            .iter()
            .filter_map(|prompt| serde_json::to_string(prompt).ok())
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let temporary = self.path.with_extension("tmp");
        if std::fs::write(&temporary, body).is_err() {
            return;
        }
        Self::restrict(&temporary);
        if std::fs::rename(&temporary, &self.path).is_err() {
            let _removed = std::fs::remove_file(&temporary);
        }
    }

    fn open_appending(&self) -> std::io::Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&self.path)?;
        Self::restrict(&self.path);
        Ok(file)
    }

    /// Prompts are the user's words and can carry anything they pasted, so
    /// the file is owner-only wherever the platform can say so.
    fn restrict(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _restricted =
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        let _unused = path;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn prompts_survive_a_restart_including_multiline_ones() {
        let home = tempfile::tempdir().unwrap();
        let store = PromptHistoryStore::new(home.path().join("history"));
        assert!(
            store.load().is_empty(),
            "a missing file is an empty history"
        );

        store.append("first");
        store.append("fix this:\nfn a() {}\n  b();");
        store.append("   ");
        assert_eq!(
            store.load(),
            [
                "first".to_owned(),
                "fix this:\nfn a() {}\n  b();".to_owned()
            ],
            "blank input is not a prompt; a multi-line prompt stays one entry"
        );

        // A fresh store over the same file is a fresh process.
        assert_eq!(PromptHistoryStore::new(store.path()).load().len(), 2);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "prompts are the user's own words");
        }
    }

    #[test]
    fn the_file_is_bounded_and_a_corrupt_line_costs_only_itself() {
        let home = tempfile::tempdir().unwrap();
        let store = PromptHistoryStore::new(home.path().join("history"));
        for index in 0..MAX_PROMPT_HISTORY + 20 {
            store.append(&format!("prompt-{index}"));
        }
        let loaded = store.load();
        assert_eq!(loaded.len(), MAX_PROMPT_HISTORY);
        assert_eq!(loaded.first().unwrap(), "prompt-20", "the oldest fall off");
        assert_eq!(
            loaded.last().unwrap(),
            &format!("prompt-{}", MAX_PROMPT_HISTORY + 19)
        );
        assert!(
            std::fs::read_to_string(store.path())
                .unwrap()
                .lines()
                .count()
                <= MAX_PROMPT_HISTORY,
            "the file itself is trimmed, not just the view"
        );

        let mut raw = std::fs::read_to_string(store.path()).unwrap();
        raw.push_str("{not json at all\n");
        std::fs::write(store.path(), raw).unwrap();
        assert_eq!(
            store.load().len(),
            MAX_PROMPT_HISTORY,
            "an unreadable line is skipped, not a reason to lose the rest"
        );
    }
}
