//! N05 effect-owned provider request-transform registry.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_core::{Context, CoreError, Layer, Next, Plugin, ProviderRequestOption};
use thiserror::Error;

use crate::{CapabilitySupport, PriceCurrency, ProviderRequestDecision, RequestDraft};

const MAX_ID_BYTES: usize = 128;
const MAX_PROVIDER_BYTES: usize = 64;

/// Validated globally unique request-transform id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestTransformId(String);

impl RequestTransformId {
    /// Validate provider:transform identity.
    ///
    /// # Errors
    /// Empty, oversized or unsafe identifiers are rejected.
    pub fn new(value: impl Into<String>) -> Result<Self, RequestTransformError> {
        let value = value.into();
        let valid = value.split_once(':').is_some_and(|(provider, transform)| {
            !transform.contains(':')
                && safe_provider(provider)
                && safe_component(transform)
                && value.len() <= MAX_ID_BYTES
        });
        if valid {
            Ok(Self(value))
        } else {
            Err(RequestTransformError::InvalidId)
        }
    }

    /// Exact id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestTransformId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What a request transform can materially change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestTransformEffect {
    /// Rewrites/truncates prompt content and may change upstream routing.
    RewritePromptAndRoute,
    /// Rewrites a provider response before the client receives it.
    RewriteResponse,
    /// Parses request documents into model-consumable content.
    ParseDocuments,
}

/// What heycode explicitly requested for one transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestTransformRequest {
    /// Explicitly disabled on this request policy.
    Disabled,
    /// Explicitly enabled on this request policy.
    Enabled,
}

/// Exact published fee per thousand parsed pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestTransformPagePrice {
    currency: PriceCurrency,
    pico_units: u64,
}

impl RequestTransformPagePrice {
    /// Currency.
    #[must_use]
    pub const fn currency(self) -> PriceCurrency {
        self.currency
    }

    /// Exact non-zero pico-units per thousand pages.
    #[must_use]
    pub const fn pico_units(self) -> u64 {
        self.pico_units
    }
}

/// Provider-published transform cost evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestTransformCost {
    /// No attributable price is published or the provider chooses dynamically.
    Unknown,
    /// Provider explicitly documents no fee.
    DocumentedFree,
    /// Covered by ordinary upstream input-token billing.
    UpstreamInputTokens,
    /// Separate exact fee per thousand pages.
    PublishedPerThousandPages(RequestTransformPagePrice),
}

impl RequestTransformCost {
    /// Construct an exact non-zero per-thousand-page fee.
    ///
    /// # Errors
    /// Zero is rejected; free and unknown have separate variants.
    pub fn published_per_thousand_pages(
        currency: PriceCurrency,
        pico_units: u64,
    ) -> Result<Self, RequestTransformError> {
        if pico_units == 0 {
            return Err(RequestTransformError::InvalidCost);
        }
        Ok(Self::PublishedPerThousandPages(RequestTransformPagePrice {
            currency,
            pico_units,
        }))
    }
}

/// One inspectable transform policy row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTransformDescriptor {
    id: RequestTransformId,
    provider: String,
    effect: RequestTransformEffect,
    request: RequestTransformRequest,
    effective: CapabilitySupport,
    cost: Option<RequestTransformCost>,
}

impl RequestTransformDescriptor {
    /// Validate one transform row.
    ///
    /// # Errors
    /// Invalid provider ownership or inconsistent request/cost evidence.
    pub fn new(
        id: RequestTransformId,
        provider: impl Into<String>,
        effect: RequestTransformEffect,
        request: RequestTransformRequest,
        effective: CapabilitySupport,
        cost: Option<RequestTransformCost>,
    ) -> Result<Self, RequestTransformError> {
        let provider = provider.into();
        if !safe_provider(&provider) || !id.as_str().starts_with(&format!("{provider}:")) {
            return Err(RequestTransformError::InvalidProvider);
        }
        match (request, cost) {
            (RequestTransformRequest::Disabled, None)
            | (RequestTransformRequest::Enabled, Some(_)) => {}
            _ => return Err(RequestTransformError::InvalidCost),
        }
        Ok(Self {
            id,
            provider,
            effect,
            request,
            effective,
            cost,
        })
    }

    /// Stable transform id.
    #[must_use]
    pub const fn id(&self) -> &RequestTransformId {
        &self.id
    }

    /// Provider owner.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Material effect.
    #[must_use]
    pub const fn effect(&self) -> RequestTransformEffect {
        self.effect
    }

    /// Explicit requested state.
    #[must_use]
    pub const fn request(&self) -> RequestTransformRequest {
        self.request
    }

    /// Evidence for what actually ran.
    #[must_use]
    pub const fn effective(&self) -> CapabilitySupport {
        self.effective
    }

    /// Cost evidence; absent only while disabled.
    #[must_use]
    pub const fn cost(&self) -> Option<RequestTransformCost> {
        self.cost
    }
}

/// One provider-owned complete transform policy.
pub trait RequestTransformProvider: Send + Sync {
    /// Exact provider id.
    fn provider(&self) -> &str;

    /// Complete transform rows, enabled and disabled.
    ///
    /// # Errors
    /// Provider-owned descriptor construction failure.
    fn descriptors(&self) -> Result<Vec<RequestTransformDescriptor>, RequestTransformError>;

    /// Materialize the exact provider option for one request.
    ///
    /// # Errors
    /// Provider-specific prerequisites or option construction failure.
    fn provider_option(
        &self,
        draft: &RequestDraft,
    ) -> Result<ProviderRequestOption, RequestTransformError>;
}

struct TransformEntry {
    provider: Arc<dyn RequestTransformProvider>,
    descriptors: Vec<RequestTransformDescriptor>,
    token: Arc<()>,
}

#[derive(Default)]
struct TransformState {
    providers: BTreeMap<String, TransformEntry>,
}

/// Effect-owned request-transform providers and deterministic inspection.
#[derive(Clone, Default)]
pub struct RequestTransformRegistry {
    state: Arc<Mutex<TransformState>>,
}

impl RequestTransformRegistry {
    /// Register one complete provider transform policy as a Context effect.
    ///
    /// # Errors
    /// Invalid/mismatched/duplicate identities or unavailable registry state.
    pub fn register(
        &self,
        context: &Context,
        provider: Arc<dyn RequestTransformProvider>,
    ) -> Result<(), RequestTransformError> {
        let provider_id = provider.provider().to_owned();
        if !safe_provider(&provider_id) {
            return Err(RequestTransformError::InvalidProvider);
        }
        let descriptors = provider.descriptors()?;
        if descriptors.is_empty() {
            return Err(RequestTransformError::EmptyProvider);
        }
        let mut ids = BTreeSet::new();
        for descriptor in &descriptors {
            if descriptor.provider() != provider_id || !ids.insert(descriptor.id().clone()) {
                return Err(RequestTransformError::InvalidDescriptor);
            }
        }
        let token = Arc::new(());
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| RequestTransformError::Unavailable)?;
            if state.providers.contains_key(&provider_id) {
                return Err(RequestTransformError::DuplicateProvider {
                    provider: provider_id,
                });
            }
            for entry in state.providers.values() {
                if entry.descriptors.iter().any(|row| ids.contains(row.id())) {
                    return Err(RequestTransformError::DuplicateTransform);
                }
            }
            state.providers.insert(
                provider_id.clone(),
                TransformEntry {
                    provider,
                    descriptors,
                    token: token.clone(),
                },
            );
        }
        let state = Arc::downgrade(&self.state);
        context.effect(move || {
            let Some(state) = state.upgrade() else {
                return;
            };
            let Ok(mut state) = state.lock() else {
                return;
            };
            if state
                .providers
                .get(&provider_id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.token, &token))
            {
                state.providers.remove(&provider_id);
            }
        });
        Ok(())
    }

    /// Stable provider/id-sorted descriptor snapshot.
    ///
    /// # Errors
    /// Unavailable state or invalid live provider rows.
    pub fn descriptors(&self) -> Result<Vec<RequestTransformDescriptor>, RequestTransformError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RequestTransformError::Unavailable)?;
        let mut rows = state
            .providers
            .values()
            .flat_map(|entry| entry.descriptors.iter().cloned())
            .collect::<Vec<_>>();
        for row in &rows {
            if state
                .providers
                .get(row.provider())
                .is_none_or(|entry| entry.provider.provider() != row.provider())
            {
                return Err(RequestTransformError::InvalidDescriptor);
            }
        }
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(rows)
    }

    /// Insert or verify the selected provider's exact durable request option.
    ///
    /// # Errors
    /// Materialization failure or conflicting same-kind durable option.
    pub fn apply(&self, draft: &mut RequestDraft) -> Result<(), RequestTransformError> {
        let Some(option) = self.option_for(draft)? else {
            return Ok(());
        };
        apply_option(&mut draft.provider_options, option)
    }

    fn option_for(
        &self,
        draft: &RequestDraft,
    ) -> Result<Option<ProviderRequestOption>, RequestTransformError> {
        let provider = {
            let state = self
                .state
                .lock()
                .map_err(|_| RequestTransformError::Unavailable)?;
            state
                .providers
                .get(&draft.provider)
                .map(|entry| entry.provider.clone())
        };
        let Some(provider) = provider else {
            return Ok(None);
        };
        let option = provider.provider_option(draft)?;
        if option.provider() != draft.provider {
            return Err(RequestTransformError::InvalidProviderOption);
        }
        Ok(Some(option))
    }
}

fn apply_option(
    options: &mut Vec<ProviderRequestOption>,
    option: ProviderRequestOption,
) -> Result<(), RequestTransformError> {
    match options
        .iter()
        .find(|current| current.provider() == option.provider() && current.kind() == option.kind())
    {
        Some(current) if current == &option => Ok(()),
        Some(_) => Err(RequestTransformError::ConflictingProviderOption {
            provider: option.provider().to_owned(),
            kind: option.kind().to_owned(),
        }),
        None => {
            options.push(option);
            Ok(())
        }
    }
}

struct RequestTransformLayer {
    registry: RequestTransformRegistry,
}

#[async_trait]
impl Layer<ProviderRequestDecision> for RequestTransformLayer {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        mut next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        next.run(input).await?;
        let Some(option) = self.registry.option_for(input.draft())? else {
            return Ok(());
        };
        apply_option(input.provider_options_mut(), option)?;
        Ok(())
    }
}

/// Publish the request-transform registry and attach its P10 layer.
#[must_use]
pub fn request_transforms_plugin() -> Box<dyn Plugin> {
    struct RequestTransformsPlugin;

    impl Plugin for RequestTransformsPlugin {
        fn name(&self) -> &'static str {
            "request-transforms"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Waterfall,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::InterceptionLayer,
                "provider/request:transforms",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_PROVIDER_INTERCEPTION]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_REQUEST_TRANSFORMS]
        }

        fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
            let interception = context
                .get::<crate::ProviderInterception>(crate::SERVICE_PROVIDER_INTERCEPTION)
                .ok_or_else(|| CoreError::other("provider interception missing"))?;
            context.provide(
                crate::SERVICE_REQUEST_TRANSFORMS,
                self.name(),
                RequestTransformRegistry::default(),
            )?;
            let registry = context
                .get::<RequestTransformRegistry>(crate::SERVICE_REQUEST_TRANSFORMS)
                .ok_or_else(|| CoreError::other("request transform registry missing"))?;
            interception.register_request(
                context,
                RequestTransformLayer {
                    registry: (*registry).clone(),
                },
            );
            Ok(())
        }
    }

    Box::new(RequestTransformsPlugin)
}

/// Request-transform validation, registration and application failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestTransformError {
    /// Transform id syntax is invalid.
    #[error("request transform id is invalid")]
    InvalidId,
    /// Provider id/ownership is invalid.
    #[error("request transform provider is invalid")]
    InvalidProvider,
    /// Provider contributed no inspectable rows.
    #[error("request transform provider has no rows")]
    EmptyProvider,
    /// Provider returned inconsistent or duplicate descriptors.
    #[error("request transform descriptor is invalid")]
    InvalidDescriptor,
    /// One provider already owns this provider policy.
    #[error("request transform provider is already registered")]
    DuplicateProvider {
        /// Contested safe provider id.
        provider: String,
    },
    /// One transform id is owned twice.
    #[error("request transform id is already registered")]
    DuplicateTransform,
    /// Registry lock/state is unavailable.
    #[error("request transform registry is unavailable")]
    Unavailable,
    /// Cost evidence is invalid.
    #[error("request transform cost evidence is invalid")]
    InvalidCost,
    /// Provider returned an invalid option envelope.
    #[error("request transform provider option is invalid")]
    InvalidProviderOption,
    /// Durable same-kind option differs from registry policy.
    #[error("request transform option conflicts with the durable request")]
    ConflictingProviderOption {
        /// Safe provider id.
        provider: String,
        /// Safe provider-option kind.
        kind: String,
    },
    /// Provider-specific request prerequisites are not met.
    #[error("request transform policy is incompatible with this request")]
    IncompatibleRequest,
}

fn safe_provider(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PROVIDER_BYTES
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}
