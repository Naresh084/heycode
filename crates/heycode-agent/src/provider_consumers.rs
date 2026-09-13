//! P10 migrations for current authentication, native-tool and telemetry users.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::{Context, CoreError, CoreResult, Layer, Next, Plugin};
use heycode_llm::{
    AuthenticationBinding, ProviderInterceptionCode, ProviderRequestDecision,
    ProviderResponseDecision, ProviderResponseItem,
};

const AUTHENTICATION_MISMATCH: &str = "authentication-binding-mismatch";
const NATIVE_TOOL_POLICY_UNAVAILABLE: &str = "native-tool-policy-unavailable";
const NATIVE_TOOL_ROUTE_DRIFT: &str = "native-tool-route-drift";

pub(crate) struct AuthenticationRequestLayer {
    providers: Arc<heycode_llm::ProviderRegistry>,
}

impl AuthenticationRequestLayer {
    pub(crate) fn new(providers: Arc<heycode_llm::ProviderRegistry>) -> Self {
        Self { providers }
    }
}

#[async_trait]
impl Layer<ProviderRequestDecision> for AuthenticationRequestLayer {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        mut next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        next.run(input).await?;
        let context = input.context();
        let Some(provider) = self.providers.get(context.provider()) else {
            input.reject(policy_code(AUTHENTICATION_MISMATCH)?);
            return Ok(());
        };
        if provider.credential_reference().is_some()
            && matches!(
                context.authentication(),
                AuthenticationBinding::None | AuthenticationBinding::Ambient
            )
        {
            input.reject(policy_code(AUTHENTICATION_MISMATCH)?);
        }
        Ok(())
    }
}

pub(crate) struct NativeToolRequestLayer {
    registry: Arc<heycode_native_tools::NativeToolRegistry>,
}

impl NativeToolRequestLayer {
    pub(crate) fn new(registry: Arc<heycode_native_tools::NativeToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Layer<ProviderRequestDecision> for NativeToolRequestLayer {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        mut next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        next.run(input).await?;
        if let Some(code) = native_tool_admission_code(
            &self.registry,
            input.draft().provider.as_str(),
            input.draft().model.as_str(),
            &input.draft().native_tool_routes,
        ) {
            input.reject(policy_code(code)?);
        }
        Ok(())
    }
}

pub(crate) fn native_tool_admission_code(
    registry: &heycode_native_tools::NativeToolRegistry,
    provider: &str,
    model: &str,
    proposed: &[heycode_core::NativeToolRoute],
) -> Option<&'static str> {
    let selected = match registry.resolve_for_model(provider, model) {
        Ok(selected) => selected,
        Err(_) => return Some(NATIVE_TOOL_POLICY_UNAVAILABLE),
    };
    if proposed
        .iter()
        .all(|route| selected.iter().any(|candidate| candidate == route))
    {
        None
    } else {
        Some(NATIVE_TOOL_ROUTE_DRIFT)
    }
}

struct ProviderTelemetryLayer {
    telemetry: Arc<heycode_telemetry::TelemetryService>,
}

#[async_trait]
impl Layer<ProviderResponseDecision> for ProviderTelemetryLayer {
    async fn handle(
        &self,
        input: &mut ProviderResponseDecision,
        mut next: Next<'_, ProviderResponseDecision>,
    ) -> anyhow::Result<()> {
        next.run(input).await?;
        let ProviderResponseItem::Failure(class) = input.item() else {
            return Ok(());
        };
        let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
            return Ok(());
        };
        let Ok(at_unix_ms) = u64::try_from(elapsed.as_millis()) else {
            return Ok(());
        };
        let Some(event) = failure_event(input.context(), *class, at_unix_ms) else {
            return Ok(());
        };
        self.telemetry.record(&event);
        Ok(())
    }
}

fn failure_event(
    context: &heycode_llm::ProviderResponseContext,
    class: heycode_llm::ProviderErrorClass,
    at_unix_ms: u64,
) -> Option<heycode_telemetry::TelemetryEvent> {
    use heycode_telemetry::{Dimension, Label, TelemetryEvent, TelemetryEventName};

    let provider = Label::new(context.provider()).ok()?;
    let outcome = Label::new(class.as_str()).ok()?;
    let mut event = TelemetryEvent::new(TelemetryEventName::RequestFailed, at_unix_ms)
        .with_dimension(Dimension::Provider, provider)
        .ok()?
        .with_dimension(Dimension::Outcome, outcome)
        .ok()?;
    if let Ok(model) = Label::new(context.model()) {
        event = event.with_dimension(Dimension::Model, model).ok()?;
    }
    Some(event)
}

/// Contribute the optional telemetry response layer.
#[must_use]
pub fn provider_telemetry_plugin() -> Box<dyn Plugin> {
    struct ProviderTelemetryPlugin;

    impl Plugin for ProviderTelemetryPlugin {
        fn name(&self) -> &'static str {
            "provider-telemetry"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Waterfall],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::InterceptionLayer,
                "provider/response:telemetry",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
                heycode_telemetry::SERVICE_TELEMETRY,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let interception = context
                .get::<heycode_llm::ProviderInterception>(
                    heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
                )
                .ok_or_else(|| CoreError::other("provider interception missing"))?;
            let telemetry = context
                .get::<heycode_telemetry::TelemetryService>(heycode_telemetry::SERVICE_TELEMETRY)
                .ok_or_else(|| CoreError::other("telemetry missing"))?;
            interception.register_response(context, ProviderTelemetryLayer { telemetry });
            Ok(())
        }
    }

    Box::new(ProviderTelemetryPlugin)
}

fn policy_code(value: &'static str) -> anyhow::Result<ProviderInterceptionCode> {
    ProviderInterceptionCode::new(value)
        .map_err(|_| anyhow::anyhow!("built-in provider policy code is invalid"))
}
