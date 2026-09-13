//! Trusted release notes bundled with the executable; no remote source or inference.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandDescriptor, CommandMetadataError, CommandSource, CommandTiming,
};

/// The changelog for this compiled snapshot, never loaded from the active workspace.
pub const BUNDLED_RELEASE_NOTES: &str = include_str!("../../../CHANGELOG.md");

/// A read-only command whose content cannot change with cwd or project trust.
///
/// # Errors
/// Invalid command metadata.
pub fn command(source: CommandSource) -> Result<Arc<dyn Command>, CommandMetadataError> {
    Ok(Arc::new(ReleaseNotes(CommandDescriptor::new(
        "release-notes",
        "Point at the changelog bundled with this build, or print it with `full`",
        vec![heycode_agent::CommandArgument::optional(
            "full",
            "Print the whole bundled changelog instead of the pointer line",
        )?],
        CommandTiming::Immediate,
        source,
    )?)))
}

/// The one-line pointer the command prints by default.
///
/// Naming the bundled file is truthful for every build; a remote changelog URL
/// would not be, because this text is compiled in and can be ahead of, or
/// behind, anything published.
#[must_use]
pub fn pointer_line() -> String {
    format!(
        "See the full changelog at: CHANGELOG.md bundled with heycode {} \
         — run `/release-notes full` to print it here",
        env!("CARGO_PKG_VERSION")
    )
}

struct ReleaseNotes(CommandDescriptor);

#[async_trait]
impl Command for ReleaseNotes {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.0
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let text = match args.trim() {
            "" => pointer_line(),
            "full" => BUNDLED_RELEASE_NOTES.to_owned(),
            _ => anyhow::bail!("Usage: /release-notes [full]"),
        };
        agent.ui().emit(heycode_agent::UiEvent::Info { text });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BUNDLED_RELEASE_NOTES, pointer_line};

    #[test]
    fn the_default_answer_is_one_line_that_names_the_bundled_file() {
        let line = pointer_line();
        assert_eq!(line.lines().count(), 1, "{line}");
        assert!(
            line.starts_with("See the full changelog at: CHANGELOG.md"),
            "{line}"
        );
        assert!(line.contains("/release-notes full"), "{line}");
        assert!(
            BUNDLED_RELEASE_NOTES.lines().count() > 1,
            "the whole changelog stays available behind `full`"
        );
    }
}
