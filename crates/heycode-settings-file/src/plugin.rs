//! File settings provider plugin.

use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_settings::{SERVICE_SETTINGS, SettingsService, SettingsWriter};

use crate::{FileSettingsConfig, FileSettingsProvider, PinnedSettingsOverlay};

/// Mount the format-preserving file provider as the `settings` service.
#[must_use]
pub fn file_settings_plugin(config: FileSettingsConfig) -> Box<dyn Plugin> {
    build_file_settings_plugin(config, None)
}

/// Mount one composition-pinned overlay below native file values. The same
/// overlay participates in subsequent native-file reloads and watcher updates.
#[must_use]
pub fn file_settings_plugin_with_overlay(
    config: FileSettingsConfig,
    overlay: Arc<dyn PinnedSettingsOverlay>,
) -> Box<dyn Plugin> {
    build_file_settings_plugin(config, Some(overlay))
}

fn build_file_settings_plugin(
    config: FileSettingsConfig,
    overlay: Option<Arc<dyn PinnedSettingsOverlay>>,
) -> Box<dyn Plugin> {
    struct FileSettingsPlugin(FileSettingsConfig, Option<Arc<dyn PinnedSettingsOverlay>>);
    impl Plugin for FileSettingsPlugin {
        fn name(&self) -> &'static str {
            "settings-file"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "settings-file",
                env!("CARGO_PKG_VERSION"),
                &[
                    PluginContributionKind::Service,
                    PluginContributionKind::Provider,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsProvider,
                "file",
            )]
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let mut provider = FileSettingsProvider::new(self.0.clone());
            if let Some(overlay) = &self.1 {
                provider = provider.with_pinned_overlay(overlay.clone());
            }
            let provider = Arc::new(provider);
            let documents = provider
                .load_documents()
                .map_err(|error| CoreError::other(error.to_string()))?;
            let writer: Arc<dyn SettingsWriter> = provider.clone();
            context.provide(
                SERVICE_SETTINGS,
                "settings-file",
                SettingsService::with_writer(documents, writer),
            )?;
            if self.0.watch {
                let settings = context
                    .get::<SettingsService>(SERVICE_SETTINGS)
                    .ok_or_else(|| CoreError::other("settings missing after provide"))?;
                let watch = provider
                    .start_watching((*settings).clone())
                    .map_err(|error| CoreError::other(error.to_string()))?;
                context.effect(move || drop(watch));
            }
            Ok(())
        }
    }
    Box::new(FileSettingsPlugin(config, overlay))
}
