//! Cached, cancellable workspace identity for persistent terminal chrome.
//!
//! Rendering consumes only cached values. Refreshes use the composed subprocess
//! effect so process ownership, sandboxing, deadlines, and cancellation remain
//! inside the repository's execution boundary.

use std::path::Path;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Cached repository facts shown beside the workspace path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceContext {
    /// Current branch, or a short commit id for detached HEAD.
    pub branch: String,
    /// Whether the checkout carries uncommitted work, tracked or not.
    pub dirty: bool,
    /// Read-only lookup for a pull request associated with this checkout.
    pub pull_request: PullRequestContext,
}

/// Result of the optional `gh pr view` query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestContext {
    /// An existing pull request for the current checkout.
    Found {
        /// Provider number.
        number: u64,
        /// Open/closed/merged state returned by GitHub.
        state: String,
        /// Human title.
        title: String,
        /// Browser URL.
        url: String,
    },
    /// GitHub CLI answered successfully enough to establish that no PR exists.
    None,
    /// The optional lookup could not be completed.
    Unavailable(&'static str),
}

/// Lifecycle of the single startup probe.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorkspaceContextState {
    /// Probe is running.
    #[default]
    Loading,
    /// The workspace is not a Git checkout.
    NotRepository,
    /// Git facts from the most recent probe.
    Ready(WorkspaceContext),
    /// Git itself could not be queried.
    Unavailable(&'static str),
}

#[derive(Debug)]
enum CommandFailure {
    Cancelled,
    TimedOut,
    Missing,
    Failed,
}

/// Read repository and current-PR information once.
pub async fn probe(
    subprocess: heycode_exec::SubprocessService,
    cwd: &Path,
    cancellation: CancellationToken,
) -> WorkspaceContextState {
    let repo = match output(
        &subprocess,
        "git",
        &["rev-parse", "--is-inside-work-tree"],
        cwd,
        &cancellation,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(output) if output.exit().is_success() && text(output.stdout()).trim() == "true" => true,
        Ok(_) => false,
        Err(CommandFailure::Cancelled) => return WorkspaceContextState::Loading,
        Err(CommandFailure::Missing) => {
            return WorkspaceContextState::Unavailable("git unavailable");
        }
        Err(CommandFailure::TimedOut) => {
            return WorkspaceContextState::Unavailable("git lookup timed out");
        }
        Err(CommandFailure::Failed) => {
            return WorkspaceContextState::Unavailable("git lookup failed");
        }
    };
    if !repo {
        return WorkspaceContextState::NotRepository;
    }

    let branch = match output(
        &subprocess,
        "git",
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        cwd,
        &cancellation,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(output) if output.exit().is_success() => clean(&text(output.stdout())),
        Ok(_) => match output(
            &subprocess,
            "git",
            &["rev-parse", "--short", "HEAD"],
            cwd,
            &cancellation,
            Duration::from_secs(2),
        )
        .await
        {
            Ok(output) if output.exit().is_success() => {
                format!("detached@{}", clean(&text(output.stdout())))
            }
            _ => "unknown branch".to_owned(),
        },
        Err(CommandFailure::Cancelled) => return WorkspaceContextState::Loading,
        Err(_) => "unknown branch".to_owned(),
    };

    let dirty = match output(
        &subprocess,
        "git",
        &["status", "--porcelain"],
        cwd,
        &cancellation,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(output) if output.exit().is_success() => !text(output.stdout()).trim().is_empty(),
        Err(CommandFailure::Cancelled) => return WorkspaceContextState::Loading,
        // An unreadable worktree is reported as clean: a marker we cannot
        // substantiate is worse than no marker.
        Ok(_) | Err(_) => false,
    };

    let pull_request = match output(
        &subprocess,
        "gh",
        &["pr", "view", "--json", "number,url,state,title"],
        cwd,
        &cancellation,
        Duration::from_secs(4),
    )
    .await
    {
        Ok(output) if output.exit().is_success() => parse_pull_request(output.stdout())
            .unwrap_or(PullRequestContext::Unavailable("PR response unavailable")),
        Ok(output) if no_pull_request(output.stderr().as_bytes()) => PullRequestContext::None,
        Ok(_) => PullRequestContext::Unavailable("PR lookup failed"),
        Err(CommandFailure::Missing) => PullRequestContext::Unavailable("gh unavailable"),
        Err(CommandFailure::TimedOut) => PullRequestContext::Unavailable("PR lookup timed out"),
        Err(CommandFailure::Cancelled) => return WorkspaceContextState::Loading,
        Err(CommandFailure::Failed) => PullRequestContext::Unavailable("PR lookup failed"),
    };

    WorkspaceContextState::Ready(WorkspaceContext {
        branch,
        dirty,
        pull_request,
    })
}

async fn output(
    subprocess: &heycode_exec::SubprocessService,
    program: &str,
    args: &[&str],
    cwd: &Path,
    cancellation: &CancellationToken,
    deadline: Duration,
) -> Result<heycode_exec::ProcessOutput, CommandFailure> {
    use heycode_exec::{ProcessErrorCode, ProcessExit, ProcessSpec};

    let executable = subprocess
        .resolve_program(std::ffi::OsStr::new(program))
        .map_err(|error| match error.code() {
            ProcessErrorCode::NotFound => CommandFailure::Missing,
            ProcessErrorCode::Cancelled | ProcessErrorCode::ServiceStopped => {
                CommandFailure::Cancelled
            }
            _ => CommandFailure::Failed,
        })?;
    let spec = ProcessSpec::new(executable, cwd)
        .and_then(|spec| spec.with_args(args.iter().copied()))
        .and_then(|spec| spec.with_environment(workspace_command_environment(program)))
        .and_then(|spec| spec.with_timeout(Some(deadline)))
        .and_then(|spec| spec.with_output_limit_bytes(64 * 1024))
        .map_err(|_| CommandFailure::Failed)?;
    let output = subprocess
        .output(spec, cancellation.clone())
        .await
        .map_err(|error| match error.code() {
            ProcessErrorCode::NotFound => CommandFailure::Missing,
            ProcessErrorCode::Cancelled | ProcessErrorCode::ServiceStopped => {
                CommandFailure::Cancelled
            }
            _ => CommandFailure::Failed,
        })?;
    if matches!(
        output.exit(),
        ProcessExit::TimedOut | ProcessExit::InactivityTimedOut
    ) {
        Err(CommandFailure::TimedOut)
    } else {
        Ok(output)
    }
}

fn workspace_command_environment(program: &str) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    const COMMON: &[&str] = &[
        "HOME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "PATH",
        "XDG_CONFIG_HOME",
    ];
    const GH_ONLY: &[&str] = &[
        "GH_CONFIG_DIR",
        "GH_ENTERPRISE_TOKEN",
        "GH_HOST",
        "GH_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GITHUB_TOKEN",
    ];
    let mut environment = std::env::vars_os()
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                let name = name.to_ascii_uppercase();
                COMMON.contains(&name.as_str())
                    || (program == "gh" && GH_ONLY.contains(&name.as_str()))
            })
        })
        .collect::<Vec<_>>();
    environment.extend([
        ("GH_PROMPT_DISABLED".into(), "1".into()),
        ("NO_COLOR".into(), "1".into()),
        ("PAGER".into(), "cat".into()),
    ]);
    environment
}

fn parse_pull_request(bytes: &[u8]) -> Option<PullRequestContext> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    Some(PullRequestContext::Found {
        number: value.get("number")?.as_u64()?,
        state: clean(value.get("state")?.as_str()?),
        title: clean(value.get("title")?.as_str()?),
        url: clean(value.get("url")?.as_str()?),
    })
}

fn no_pull_request(stderr: &[u8]) -> bool {
    let message = text(stderr).to_ascii_lowercase();
    message.contains("no pull requests found")
        || message.contains("could not resolve to a pullrequest")
        || message.contains("no pull request found")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn clean(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_pull_request_identity() {
        assert_eq!(
            parse_pull_request(
                br#"{"number":42,"url":"https://example.test/pull/42","state":"OPEN","title":"Repair TUI"}"#
            ),
            Some(PullRequestContext::Found {
                number: 42,
                state: "OPEN".to_owned(),
                title: "Repair TUI".to_owned(),
                url: "https://example.test/pull/42".to_owned(),
            })
        );
    }

    #[test]
    fn recognizes_only_explicit_no_pull_request_answers() {
        assert!(no_pull_request(b"no pull requests found for branch main"));
        assert!(!no_pull_request(b"network unreachable"));
    }
}
