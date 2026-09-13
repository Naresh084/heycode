//! Optional document-extraction binding and human `/document` command.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    Agent, AttachmentComposerAction, Command, CommandArgument, CommandDescriptor, CommandSource,
    CommandTiming, UiEvent,
};

struct DocumentCommand {
    descriptor: CommandDescriptor,
    store: Arc<heycode_attachments::AttachmentStore>,
    lifecycle: CancellationToken,
}

#[async_trait]
impl Command for DocumentCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let value = args.trim();
        if value.is_empty() {
            anyhow::bail!("usage: /document <path|clear>");
        }
        if value == "clear" {
            agent.ui().emit(UiEvent::AttachmentComposerRequested {
                action: AttachmentComposerAction::Clear,
            });
            return Ok(());
        }
        if self.lifecycle.is_cancelled() {
            anyhow::bail!("document command is unavailable");
        }
        let requested = std::path::PathBuf::from(value);
        let path = if requested.is_absolute() {
            requested
        } else {
            agent.cwd().join(requested)
        };
        let admission = self
            .store
            .admit_document_path(&path, self.lifecycle.child_token())
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        agent.ui().emit(UiEvent::AttachmentComposerRequested {
            action: AttachmentComposerAction::Add(admission.metadata().clone()),
        });
        Ok(())
    }
}

/// Bind the composed document extractor to Agent and contribute `/document`.
#[must_use]
pub fn agent_documents_plugin() -> Box<dyn heycode_core::Plugin> {
    struct AgentDocumentsPlugin;

    impl heycode_core::Plugin for AgentDocumentsPlugin {
        fn name(&self) -> &'static str {
            "agent-documents"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Command],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::Command,
                "document",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_AGENT,
                crate::SERVICE_COMMANDS,
                heycode_attachments::SERVICE_ATTACHMENTS,
                heycode_web::SERVICE_DOCUMENT_EXTRACTOR,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let agent = context
                .get::<Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent service type mismatch"))?;
            let commands = context
                .get::<crate::CommandRegistry>(crate::SERVICE_COMMANDS)
                .ok_or_else(|| heycode_core::CoreError::other("commands service type mismatch"))?;
            let store = context
                .get::<heycode_attachments::AttachmentStore>(
                    heycode_attachments::SERVICE_ATTACHMENTS,
                )
                .ok_or_else(|| {
                    heycode_core::CoreError::other("attachments service type mismatch")
                })?;
            let extractor = context
                .get::<heycode_web::DocumentExtractor>(heycode_web::SERVICE_DOCUMENT_EXTRACTOR)
                .ok_or_else(|| heycode_core::CoreError::other("document service type mismatch"))?;
            let binding = agent
                .install_document_extractor(extractor)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.effect(move || drop(binding));
            let lifecycle = CancellationToken::new();
            let descriptor = CommandDescriptor::new(
                "document",
                "Add a PDF/HTML document to the next message or clear pending attachments",
                vec![
                    CommandArgument::required("path", "Document path or the word clear")
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?
                        .variadic(),
                ],
                CommandTiming::Immediate,
                CommandSource::from_plugin(self.name())
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    context,
                    Arc::new(DocumentCommand {
                        descriptor,
                        store,
                        lifecycle: lifecycle.clone(),
                    }),
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.effect(move || lifecycle.cancel());
            Ok(())
        }
    }

    Box::new(AgentDocumentsPlugin)
}
