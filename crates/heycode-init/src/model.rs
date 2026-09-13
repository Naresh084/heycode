//! Opaque preview identity and safe user-facing projections.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use crate::InitError;

/// Opening marker for the only section `/init` may replace.
pub const MANAGED_START: &str = "<!-- heycode:init v1 start -->";
/// Closing marker for the only section `/init` may replace.
pub const MANAGED_END: &str = "<!-- heycode:init v1 end -->";

/// Structural change represented by one preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitChangeKind {
    /// Create a new AGENTS.md document.
    Create,
    /// Append the first managed section to existing project law.
    Append,
    /// Replace only an existing managed section.
    Refresh,
    /// Managed guidance already matches current workspace facts.
    Unchanged,
}

impl InitChangeKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Append => "append managed section",
            Self::Refresh => "refresh managed section",
            Self::Unchanged => "already current",
        }
    }
}

/// Opaque optimistic-concurrency identity for one exact proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitPreviewToken(String);

impl InitPreviewToken {
    pub(crate) fn from_hash(value: String) -> Self {
        Self(value)
    }

    /// Lowercase hexadecimal token accepted by `/init apply`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for InitPreviewToken {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for InitPreviewToken {
    type Err = InitError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(InitError::InvalidToken)
        }
    }
}

/// Bounded no-write preview for one exact workspace/file generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitPreview {
    pub(crate) change: InitChangeKind,
    pub(crate) token: InitPreviewToken,
    pub(crate) diff: String,
}

impl InitPreview {
    /// Proposed structural operation.
    #[must_use]
    pub const fn change(&self) -> InitChangeKind {
        self.change
    }

    /// Token required to apply this exact proposal.
    #[must_use]
    pub const fn token(&self) -> &InitPreviewToken {
        &self.token
    }

    /// Render the bounded human preview and explicit next action.
    #[must_use]
    pub fn render(&self) -> String {
        if self.change == InitChangeKind::Unchanged {
            return "AGENTS.md managed guidance is already current. No file changed.".to_owned();
        }
        format!(
            "AGENTS.md preview ({})\n\n{}\n\nNo file changed. Apply this exact preview with `/init apply {}`.",
            self.change.label(),
            self.diff,
            self.token
        )
    }
}

/// Durable result after a token-checked apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitApplyOutcome {
    /// A new AGENTS.md was committed.
    Created,
    /// An append or managed-section refresh committed.
    Updated,
    /// The preview was current and required no write.
    Unchanged,
}

impl InitApplyOutcome {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Created => "Created AGENTS.md from the approved preview.",
            Self::Updated => "Updated only the approved AGENTS.md managed section.",
            Self::Unchanged => "AGENTS.md managed guidance is already current.",
        }
    }
}
