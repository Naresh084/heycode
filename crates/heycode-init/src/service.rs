//! Bounded discovery, managed-section merge and atomic token-checked apply.

use std::io::{Read as _, Write as _};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use atomic_write_file::AtomicWriteFile;
use sha2::{Digest as _, Sha256};

use crate::{
    InitApplyOutcome, InitChangeKind, InitError, InitPreview, InitPreviewToken, MANAGED_END,
    MANAGED_START,
};

const MAX_AGENTS_BYTES: usize = 1024 * 1024;
const MAX_DIFF_BYTES: usize = 48 * 1024;

/// Preview/apply service bound to one canonical workspace root.
pub struct InitService {
    root: PathBuf,
    writer: Mutex<()>,
}

impl InitService {
    /// Bind to an existing workspace directory.
    ///
    /// # Errors
    /// Missing, non-directory or inaccessible roots fail before publication.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, InitError> {
        let metadata = std::fs::metadata(root.as_ref()).map_err(|_| InitError::InvalidWorkspace)?;
        if !metadata.is_dir() {
            return Err(InitError::InvalidWorkspace);
        }
        let root = std::fs::canonicalize(root.as_ref()).map_err(|_| InitError::InvalidWorkspace)?;
        Ok(Self {
            root,
            writer: Mutex::new(()),
        })
    }

    /// Compute a bounded diff and token without changing the filesystem.
    ///
    /// # Errors
    /// Unsafe/oversized/non-UTF-8 targets, malformed markers or I/O failures.
    pub fn preview(&self) -> Result<InitPreview, InitError> {
        let _guard = self
            .writer
            .lock()
            .map_err(|_| InitError::WriterUnavailable)?;
        let proposal = self.proposal()?;
        Ok(InitPreview {
            change: proposal.change,
            token: proposal.token,
            diff: proposal.diff,
        })
    }

    /// Apply only the exact currently reproducible preview.
    ///
    /// # Errors
    /// Invalid/stale tokens, unsafe targets, malformed markers or atomic-write
    /// failures. A stale token writes nothing.
    pub fn apply(&self, token: &InitPreviewToken) -> Result<InitApplyOutcome, InitError> {
        let _guard = self
            .writer
            .lock()
            .map_err(|_| InitError::WriterUnavailable)?;
        let proposal = self.proposal()?;
        if &proposal.token != token {
            return Err(InitError::StalePreview);
        }
        if proposal.change == InitChangeKind::Unchanged {
            return Ok(InitApplyOutcome::Unchanged);
        }
        let confirmed = self.proposal()?;
        if &confirmed.token != token {
            return Err(InitError::StalePreview);
        }
        self.write_document(&confirmed.proposed)?;
        Ok(if confirmed.original.is_some() {
            InitApplyOutcome::Updated
        } else {
            InitApplyOutcome::Created
        })
    }

    /// Execute the closed `/init` argument grammar and return safe UI text.
    ///
    /// # Errors
    /// Usage, token, preview or apply failures.
    pub fn execute(&self, args: &str) -> Result<String, InitError> {
        let words = args.split_whitespace().collect::<Vec<_>>();
        match words.as_slice() {
            [] | ["preview"] => self.preview().map(|preview| preview.render()),
            ["apply", raw] => {
                let token = raw.parse::<InitPreviewToken>()?;
                self.apply(&token)
                    .map(|outcome| outcome.message().to_owned())
            }
            _ => Err(InitError::Usage),
        }
    }

    fn proposal(&self) -> Result<Proposal, InitError> {
        let original = self.read_existing()?;
        let managed = managed_section(&self.root);
        let (proposed, change, prior_managed) = merge_document(original.as_deref(), &managed)?;
        let token = proposal_token(original.as_deref(), &proposed);
        let diff = render_diff(change, prior_managed.as_deref(), &managed);
        Ok(Proposal {
            original,
            proposed,
            change,
            token,
            diff,
        })
    }

    fn target(&self) -> PathBuf {
        self.root.join("AGENTS.md")
    }

    fn read_existing(&self) -> Result<Option<String>, InitError> {
        let target = self.target();
        let metadata = match std::fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(InitError::io("inspect", source)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(InitError::UnsafeTarget);
        }
        if metadata.len() > u64::try_from(MAX_AGENTS_BYTES).unwrap_or(u64::MAX) {
            return Err(InitError::TooLarge {
                max_bytes: MAX_AGENTS_BYTES,
            });
        }
        let file = std::fs::File::open(&target).map_err(|source| InitError::io("read", source))?;
        let mut bytes = Vec::new();
        file.take(u64::try_from(MAX_AGENTS_BYTES + 1).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(|source| InitError::io("read", source))?;
        if bytes.len() > MAX_AGENTS_BYTES {
            return Err(InitError::TooLarge {
                max_bytes: MAX_AGENTS_BYTES,
            });
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| InitError::InvalidUtf8)
    }

    fn write_document(&self, document: &str) -> Result<(), InitError> {
        let target = self.target();
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(InitError::UnsafeTarget);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(InitError::io("inspect", source)),
        }
        let mut options = AtomicWriteFile::options();
        #[cfg(unix)]
        {
            use atomic_write_file::unix::OpenOptionsExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.preserve_mode(true).mode(0o644);
        }
        let mut output = options
            .open(&target)
            .map_err(|source| InitError::io("write", source))?;
        output
            .write_all(document.as_bytes())
            .map_err(|source| InitError::io("write", source))?;
        output
            .commit()
            .map_err(|source| InitError::io("commit", source))
    }
}

struct Proposal {
    original: Option<String>,
    proposed: String,
    change: InitChangeKind,
    token: InitPreviewToken,
    diff: String,
}

fn managed_section(root: &Path) -> String {
    let mut ecosystems = Vec::new();
    let mut verification = Vec::new();
    if root.join("Cargo.toml").is_file() {
        ecosystems.push("Rust/Cargo (`Cargo.toml`)");
        verification.extend([
            "`cargo fmt --all --check`",
            "`cargo clippy --workspace --all-targets -- -D warnings`",
            "`cargo test --workspace`",
        ]);
    }
    if root.join("go.mod").is_file() {
        ecosystems.push("Go (`go.mod`)");
        verification.push("`go test ./...`");
    }
    if root.join("package.json").is_file() {
        ecosystems.push(
            "Node.js (`package.json`; inspect scripts and the lockfile before choosing commands)",
        );
    }
    if root.join("pyproject.toml").is_file() {
        ecosystems.push("Python (`pyproject.toml`; use its declared tool configuration)");
    }
    if root.join("Makefile").is_file() {
        ecosystems.push("Make (`Makefile`; inspect documented targets before execution)");
    }
    if ecosystems.is_empty() {
        ecosystems.push("No standard root manifest detected; inspect README and CI configuration");
    }

    let mut lines = vec![
        MANAGED_START.to_owned(),
        "## heycode workspace guidance".to_owned(),
        String::new(),
        "This section is managed by `/init`. Keep project-specific rules outside these markers."
            .to_owned(),
        String::new(),
        "- Read every applicable `AGENTS.md` before editing; deeper files take precedence."
            .to_owned(),
        "- Preserve user-authored instructions and unrelated working-tree changes.".to_owned(),
        "- Keep implementation and verification scoped to the requested feature.".to_owned(),
        format!("- Detected workspace: {}.", ecosystems.join(", ")),
    ];
    if verification.is_empty() {
        lines.push(
            "- Verification: determine the repository's documented commands before changing files."
                .to_owned(),
        );
    } else {
        lines.push(format!(
            "- Verification: run {} before declaring the change complete.",
            verification.join(", ")
        ));
    }
    lines.push(MANAGED_END.to_owned());
    lines.join("\n")
}

fn merge_document(
    existing: Option<&str>,
    managed: &str,
) -> Result<(String, InitChangeKind, Option<String>), InitError> {
    let Some(existing) = existing else {
        return Ok((
            format!("# AGENTS.md\n\n{managed}\n"),
            InitChangeKind::Create,
            None,
        ));
    };
    let Some(range) = managed_range(existing)? else {
        let separator = if existing.is_empty() || existing.ends_with("\n\n") {
            ""
        } else if existing.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        return Ok((
            format!("{existing}{separator}{managed}\n"),
            InitChangeKind::Append,
            None,
        ));
    };
    let prior = existing[range.clone()].to_owned();
    let proposed = format!(
        "{}{}{}",
        &existing[..range.start],
        managed,
        &existing[range.end..]
    );
    let change = if proposed == existing {
        InitChangeKind::Unchanged
    } else {
        InitChangeKind::Refresh
    };
    Ok((proposed, change, Some(prior)))
}

fn managed_range(document: &str) -> Result<Option<Range<usize>>, InitError> {
    let starts = document
        .match_indices(MANAGED_START)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let ends = document
        .match_indices(MANAGED_END)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if starts.is_empty() && ends.is_empty() {
        return Ok(None);
    }
    if starts.len() != 1 || ends.len() != 1 {
        return Err(InitError::MalformedManagedSection);
    }
    let start = starts[0];
    let end = ends[0];
    if end < start.saturating_add(MANAGED_START.len()) {
        return Err(InitError::MalformedManagedSection);
    }
    Ok(Some(start..end.saturating_add(MANAGED_END.len())))
}

fn proposal_token(existing: Option<&str>, proposed: &str) -> InitPreviewToken {
    let mut hash = Sha256::new();
    hash.update(b"dshx-init-v1\0");
    match existing {
        Some(existing) => {
            hash.update(b"present\0");
            hash.update(existing.as_bytes());
        }
        None => hash.update(b"absent\0"),
    }
    hash.update(b"\0proposal\0");
    hash.update(proposed.as_bytes());
    let digest = hash.finalize();
    let mut token = String::with_capacity(32);
    for byte in &digest[..16] {
        token.push_str(&format!("{byte:02x}"));
    }
    InitPreviewToken::from_hash(token)
}

fn render_diff(change: InitChangeKind, prior: Option<&str>, managed: &str) -> String {
    let mut output = String::from("--- AGENTS.md\n+++ AGENTS.md (preview)\n");
    match change {
        InitChangeKind::Create => {
            push_prefixed(&mut output, "+# AGENTS.md", MAX_DIFF_BYTES);
            push_prefixed(&mut output, "+", MAX_DIFF_BYTES);
            for line in managed.lines() {
                push_prefixed(&mut output, &format!("+{line}"), MAX_DIFF_BYTES);
            }
        }
        InitChangeKind::Append => {
            for line in managed.lines() {
                push_prefixed(&mut output, &format!("+{line}"), MAX_DIFF_BYTES);
            }
        }
        InitChangeKind::Refresh => {
            for line in prior.unwrap_or_default().lines() {
                push_prefixed(&mut output, &format!("-{line}"), MAX_DIFF_BYTES);
            }
            for line in managed.lines() {
                push_prefixed(&mut output, &format!("+{line}"), MAX_DIFF_BYTES);
            }
        }
        InitChangeKind::Unchanged => {}
    }
    output.trim_end().to_owned()
}

fn push_prefixed(output: &mut String, line: &str, cap: usize) {
    if output.ends_with("… diff truncated …\n") {
        return;
    }
    if output.len().saturating_add(line.len()).saturating_add(1) > cap {
        output.push_str("… diff truncated …\n");
        return;
    }
    output.push_str(line);
    output.push('\n');
}
