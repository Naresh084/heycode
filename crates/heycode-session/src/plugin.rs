//! Plugin constructor publishing service `"session"`.

use heycode_core::{Context, CoreError, CoreResult, Plugin};

use crate::{SERVICE_SESSION, Session};

/// Create a fresh session under `sessions_dir` and publish it as
/// `Arc<std::sync::Mutex<Session>>` under `"session"`.
pub fn session_plugin(sessions_dir: std::path::PathBuf) -> Box<dyn Plugin> {
    struct SessionPlugin(std::path::PathBuf);
    impl Plugin for SessionPlugin {
        fn name(&self) -> &'static str {
            "session"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "session",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SESSION]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let session = Session::create(&self.0)
                .map_err(|e| CoreError::other(format!("failed to create session: {e}")))?;
            ctx.provide(SERVICE_SESSION, "session", std::sync::Mutex::new(session))
        }
    }
    Box::new(SessionPlugin(sessions_dir))
}

/// Create a fresh metadata-bearing session and publish it as "session".
///
/// This is the production constructor for callers that know cwd/runtime/source;
/// the compatibility constructor retains unknown facts.
#[must_use]
pub fn session_with_metadata_plugin(
    sessions_dir: std::path::PathBuf,
    metadata: crate::SessionCreationMetadata,
) -> Box<dyn Plugin> {
    struct MetadataSessionPlugin(std::path::PathBuf, crate::SessionCreationMetadata);

    impl Plugin for MetadataSessionPlugin {
        fn name(&self) -> &'static str {
            "session"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SESSION]
        }

        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let session = Session::create_with_metadata(&self.0, self.1.clone())
                .map_err(|error| CoreError::other(format!("failed to create session: {error}")))?;
            ctx.provide(SERVICE_SESSION, self.name(), std::sync::Mutex::new(session))
        }
    }

    Box::new(MetadataSessionPlugin(sessions_dir, metadata))
}

/// Create a fresh metadata-bearing session using one caller-minted id.
///
/// This lets composition bind other session-scoped product adapters before
/// plugin application while retaining create-new/no-clobber session storage.
#[must_use]
pub fn session_with_id_and_metadata_plugin(
    sessions_dir: std::path::PathBuf,
    session_id: heycode_core::SessionId,
    metadata: crate::SessionCreationMetadata,
) -> Box<dyn Plugin> {
    struct ExactSessionPlugin(
        std::path::PathBuf,
        heycode_core::SessionId,
        crate::SessionCreationMetadata,
    );

    impl Plugin for ExactSessionPlugin {
        fn name(&self) -> &'static str {
            "session"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SESSION]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let session =
                Session::create_with_id_and_metadata(&self.0, self.1.clone(), self.2.clone())
                    .map_err(|error| {
                        CoreError::other(format!("failed to create session: {error}"))
                    })?;
            context.provide(SERVICE_SESSION, self.name(), std::sync::Mutex::new(session))
        }
    }

    Box::new(ExactSessionPlugin(sessions_dir, session_id, metadata))
}

/// Reopen an existing session and publish it as `"session"`.
///
/// Accepts either the `session.jsonl` FILE itself or its containing session
/// directory; both resolve to the same log.
///
/// # Errors surfaced at apply time
/// Missing/corrupt logs fail composition loudly (AGENTS.md fail-loud rule), as
/// does a log another live heycode process already owns for writing.
pub fn session_resume_plugin(session_path: std::path::PathBuf) -> Box<dyn Plugin> {
    struct ResumePlugin(std::path::PathBuf);
    impl Plugin for ResumePlugin {
        fn name(&self) -> &'static str {
            "session-resume"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "session-resume",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SESSION]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            // Session::open expects the containing directory.
            let dir = if self.0.is_file() {
                self.0
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|| self.0.clone())
            } else {
                self.0.clone()
            };
            // The resumed handle is the world's writer, so it takes the log's
            // exclusive lease: a second heycode resuming the same session would
            // otherwise mint the same sequence numbers and destroy the log.
            let session = Session::open_for_writing(&dir)
                .map_err(|e| CoreError::other(format!("failed to resume session: {e}")))?;
            ctx.provide(SERVICE_SESSION, "session", std::sync::Mutex::new(session))
        }
    }
    Box::new(ResumePlugin(session_path))
}

/// Publish the bounded local JSONL-truth query and lifecycle provider.
#[must_use]
pub fn session_query_jsonl_plugin(sessions_dir: std::path::PathBuf) -> Box<dyn Plugin> {
    struct QueryPlugin(std::path::PathBuf);

    impl Plugin for QueryPlugin {
        fn name(&self) -> &'static str {
            "session-query-jsonl"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_SESSION_QUERY]
        }

        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let backend =
                std::sync::Arc::new(crate::query::LocalSessionQueryBackend::new(self.0.clone()));
            let active = backend.shutdown_handle();
            ctx.effect(move || {
                active.store(false, std::sync::atomic::Ordering::Release);
            });
            ctx.provide(
                crate::SERVICE_SESSION_QUERY,
                self.name(),
                crate::SessionQueryService::new(backend),
            )
        }
    }

    Box::new(QueryPlugin(sessions_dir))
}
