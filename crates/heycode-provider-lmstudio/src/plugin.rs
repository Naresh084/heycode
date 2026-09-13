//! Provider-owned LM Studio detection plugin.

use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor, ServiceKey,
};
use heycode_credentials::{CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpService, SERVICE_HTTP};

use crate::config::LmStudioConfig;
use crate::detect::LmStudioDetector;
use crate::endpoint::LmStudioAuth;
use crate::model_control::LmStudioModelControl;

/// Service key for the LM Studio detector.
pub const SERVICE_LM_STUDIO: ServiceKey = ServiceKey::new("lmstudio");
/// Service key for explicit LM Studio model management.
pub const SERVICE_LM_STUDIO_MODEL_CONTROL: ServiceKey = ServiceKey::new("lmstudio/model-control");

/// Publish the LM Studio endpoint/auth/health detector.
///
/// The plugin injects credentials only when the configuration actually resolves
/// a bearer token; LM Studio's documented default posture needs none, and
/// declaring an unused dependency would make composition fail for the majority
/// of installs.
#[must_use]
pub fn lmstudio_plugin(config: LmStudioConfig) -> Box<dyn Plugin> {
    struct LmStudioPlugin(LmStudioConfig);

    impl Plugin for LmStudioPlugin {
        fn name(&self) -> &'static str {
            "provider-lmstudio"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_LM_STUDIO, SERVICE_LM_STUDIO_MODEL_CONTROL]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            match self.0.auth() {
                LmStudioAuth::None => &[SERVICE_HTTP],
                LmStudioAuth::BearerToken(_) => &[SERVICE_HTTP, SERVICE_CREDENTIALS],
            }
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = match self.0.auth() {
                LmStudioAuth::None => None,
                LmStudioAuth::BearerToken(_) => Some(
                    context
                        .get::<CredentialsService>(SERVICE_CREDENTIALS)
                        .ok_or_else(|| {
                            CoreError::MissingService(SERVICE_CREDENTIALS.to_string())
                        })?,
                ),
            };
            // The bare detector, never an `Arc`: `provide` stores the value and
            // `get::<LmStudioDetector>` would answer `None` for `Arc<Arc<_>>`
            // (GOTCHAS #27).
            let http = http.as_ref().clone();
            context.provide(
                SERVICE_LM_STUDIO,
                self.name(),
                LmStudioDetector::new(http.clone(), credentials.clone(), self.0.clone()),
            )?;
            context.provide(
                SERVICE_LM_STUDIO_MODEL_CONTROL,
                self.name(),
                LmStudioModelControl::new(http, credentials, self.0.clone()),
            )
        }
    }

    Box::new(LmStudioPlugin(config))
}
