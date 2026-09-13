//! Settings-backed provider and public-domain policy.

use std::collections::BTreeSet;

use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_settings::{SettingsDefinition, SettingsNamespace, SettingsSchema};

use crate::{SERVICE_WEB, WebRegistry, safe_id};

const MAX_DOMAIN_RULES: usize = 128;

/// Canonical allow/block rules for public URL domains.
///
/// A rule matches its exact domain and every subdomain. An empty allow list
/// permits every otherwise-valid unambiguous public host; block rules take
/// precedence. When block rules exist, IP literals are refused and an IDN
/// host must also match an IDN allow rule, because neither form can be compared
/// safely with an arbitrary ASCII blocked name.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct WebDomainPolicy {
    allow: Vec<String>,
    block: Vec<String>,
}

impl std::fmt::Debug for WebDomainPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebDomainPolicy")
            .field("allow_count", &self.allow.len())
            .field("block_count", &self.block.len())
            .finish()
    }
}

impl WebDomainPolicy {
    /// Construct canonical exact-or-subdomain rules.
    ///
    /// # Errors
    /// More than 128 entries per list, malformed domains, duplicates, or one
    /// exact domain appearing in both lists fail.
    pub fn new(allow: Vec<String>, block: Vec<String>) -> Result<Self, WebPolicyError> {
        if allow.len() > MAX_DOMAIN_RULES || block.len() > MAX_DOMAIN_RULES {
            return Err(WebPolicyError::TooManyDomains);
        }
        let allow = canonical_domains(allow)?;
        let block = canonical_domains(block)?;
        if allow
            .iter()
            .any(|domain| block.binary_search(domain).is_ok())
        {
            return Err(WebPolicyError::ConflictingDomain);
        }
        Ok(Self { allow, block })
    }

    /// Canonical allowed domains. Empty means any public host.
    #[must_use]
    pub fn allow(&self) -> &[String] {
        &self.allow
    }

    /// Canonical blocked domains, evaluated before the allow list.
    #[must_use]
    pub fn block(&self) -> &[String] {
        &self.block
    }

    /// Whether one already-shaped public HTTP(S) URL passes these rules.
    #[must_use]
    pub fn allows(&self, value: &str) -> bool {
        url::Url::parse(value)
            .ok()
            .is_some_and(|url| self.allows_url(&url))
    }

    pub(crate) fn allows_url(&self, url: &url::Url) -> bool {
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return false;
        }
        match url.host() {
            Some(url::Host::Domain(host)) => {
                let host = host.trim_end_matches('.');
                let blocked = self.block.iter().any(|rule| domain_matches(host, rule));
                let explicitly_allowed = self.allow.iter().any(|rule| domain_matches(host, rule));
                let explicitly_allowed_idn = self
                    .allow
                    .iter()
                    .any(|rule| contains_idna_label(rule) && domain_matches(host, rule));
                let idn_requires_positive_authority =
                    !self.block.is_empty() && contains_idna_label(host) && !explicitly_allowed_idn;
                !blocked
                    && !idn_requires_positive_authority
                    && (self.allow.is_empty() || explicitly_allowed)
            }
            Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) => {
                self.allow.is_empty() && self.block.is_empty()
            }
            None => false,
        }
    }

    fn from_value(value: Option<&serde_json::Value>) -> Result<Self, WebPolicyError> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let object = value.as_object().ok_or(WebPolicyError::InvalidShape)?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "allow" | "block"))
        {
            return Err(WebPolicyError::InvalidShape);
        }
        Self::new(
            string_array(object.get("allow"))?,
            string_array(object.get("block"))?,
        )
    }
}

/// Immutable search/fetch provider and domain selection.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct WebPolicy {
    search_provider: Option<String>,
    fetch_provider: Option<String>,
    search_domains: WebDomainPolicy,
    fetch_domains: WebDomainPolicy,
}

impl std::fmt::Debug for WebPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebPolicy")
            .field("search_provider", &self.search_provider)
            .field("fetch_provider", &self.fetch_provider)
            .field("search_domains", &self.search_domains)
            .field("fetch_domains", &self.fetch_domains)
            .finish()
    }
}

impl WebPolicy {
    /// Construct one validated web policy.
    ///
    /// # Errors
    /// Configured provider ids must be lowercase kebab-case.
    pub fn new(
        search_provider: Option<String>,
        fetch_provider: Option<String>,
        search_domains: WebDomainPolicy,
        fetch_domains: WebDomainPolicy,
    ) -> Result<Self, WebPolicyError> {
        if search_provider.as_deref().is_some_and(|id| !safe_id(id))
            || fetch_provider.as_deref().is_some_and(|id| !safe_id(id))
        {
            return Err(WebPolicyError::InvalidProvider);
        }
        Ok(Self {
            search_provider,
            fetch_provider,
            search_domains,
            fetch_domains,
        })
    }

    /// Explicit search provider id, or automatic unique selection.
    #[must_use]
    pub fn search_provider(&self) -> Option<&str> {
        self.search_provider.as_deref()
    }

    /// Explicit fetch provider id, or automatic unique selection.
    #[must_use]
    pub fn fetch_provider(&self) -> Option<&str> {
        self.fetch_provider.as_deref()
    }

    /// Search-result domain policy.
    #[must_use]
    pub fn search_domains(&self) -> &WebDomainPolicy {
        &self.search_domains
    }

    /// Fetch request/redirect/final-URL domain policy.
    #[must_use]
    pub fn fetch_domains(&self) -> &WebDomainPolicy {
        &self.fetch_domains
    }

    pub(crate) fn from_value(value: &serde_json::Value) -> Result<Self, WebPolicyError> {
        let object = value.as_object().ok_or(WebPolicyError::InvalidShape)?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "search_provider" | "fetch_provider" | "search_domains" | "fetch_domains"
            )
        }) {
            return Err(WebPolicyError::InvalidShape);
        }
        Self::new(
            optional_provider(object.get("search_provider"))?,
            optional_provider(object.get("fetch_provider"))?,
            WebDomainPolicy::from_value(object.get("search_domains"))?,
            WebDomainPolicy::from_value(object.get("fetch_domains"))?,
        )
    }
}

/// Stable web-policy validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebPolicyError {
    /// Top-level or nested settings shape is invalid.
    #[error("web policy shape is invalid")]
    InvalidShape,
    /// Provider id is malformed.
    #[error("web provider selection is invalid")]
    InvalidProvider,
    /// A domain rule is malformed.
    #[error("web domain rule is invalid")]
    InvalidDomain,
    /// A domain list exceeds its fixed bound.
    #[error("web domain policy has too many rules")]
    TooManyDomains,
    /// A duplicate domain rule is ambiguous.
    #[error("web domain policy contains a duplicate rule")]
    DuplicateDomain,
    /// The same exact domain is allowed and blocked.
    #[error("web domain policy contains a conflicting rule")]
    ConflictingDomain,
    /// Persisted provider is not registered for the selected operation.
    #[error("web {operation} policy selects unavailable provider `{provider}`")]
    UnavailableProvider {
        /// Search or fetch.
        operation: &'static str,
        /// Validated safe provider id.
        provider: String,
    },
}

/// Settings namespace owned by the policy plugin.
///
/// # Errors
/// Static namespace validation failure.
pub fn web_policy_namespace() -> Result<SettingsNamespace, heycode_settings::SettingsError> {
    SettingsNamespace::new("web")
}

fn definition(
    search_ids: BTreeSet<String>,
    fetch_ids: BTreeSet<String>,
) -> Result<SettingsDefinition, heycode_settings::SettingsError> {
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "properties":{
                "search_provider":{"type":["string","null"]},
                "fetch_provider":{"type":["string","null"]},
                "search_domains":domain_schema(),
                "fetch_domains":domain_schema()
            }
        }),
        serde_json::json!({
            "search_provider":null,
            "fetch_provider":null,
            "search_domains":{"allow":[],"block":[]},
            "fetch_domains":{"allow":[],"block":[]}
        }),
        move |value| {
            let policy = WebPolicy::from_value(value).map_err(|error| error.to_string())?;
            if let Some(provider) = policy
                .search_provider()
                .filter(|id| !search_ids.contains(*id))
            {
                return Err(WebPolicyError::UnavailableProvider {
                    operation: "search",
                    provider: provider.to_owned(),
                }
                .to_string());
            }
            if let Some(provider) = policy
                .fetch_provider()
                .filter(|id| !fetch_ids.contains(*id))
            {
                return Err(WebPolicyError::UnavailableProvider {
                    operation: "fetch",
                    provider: provider.to_owned(),
                }
                .to_string());
            }
            Ok(())
        },
    )?
    .with_wire_exposure();
    Ok(SettingsDefinition::new(web_policy_namespace()?, schema))
}

/// Mount live Settings-backed provider/domain policy.
#[must_use]
pub fn web_policy_plugin() -> Box<dyn Plugin> {
    struct WebPolicyPlugin;

    impl Plugin for WebPolicyPlugin {
        fn name(&self) -> &'static str {
            "web-policy"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                "web",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS, SERVICE_WEB]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings service type mismatch"))?;
            let registry = context
                .get::<WebRegistry>(SERVICE_WEB)
                .ok_or_else(|| CoreError::other("web service type mismatch"))?;
            let descriptors = registry
                .descriptors()
                .map_err(|error| CoreError::other(error.to_string()))?;
            let search_ids = descriptors
                .iter()
                .filter(|descriptor| descriptor.supports_search())
                .map(|descriptor| descriptor.id().to_owned())
                .collect();
            let fetch_ids = descriptors
                .iter()
                .filter(|descriptor| descriptor.supports_fetch())
                .map(|descriptor| descriptor.id().to_owned())
                .collect();
            let namespace =
                web_policy_namespace().map_err(|error| CoreError::other(error.to_string()))?;
            let snapshot = settings
                .register(
                    context,
                    definition(search_ids, fetch_ids)
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let initial = WebPolicy::from_value(snapshot.resolved())
                .map_err(|error| CoreError::other(error.to_string()))?;
            registry
                .replace_policy(initial)
                .map_err(|error| CoreError::other(error.to_string()))?;
            let reset = registry.clone();
            context.effect(move || reset.reset_policy());
            let watcher = registry.clone();
            settings
                .watch(
                    context,
                    &namespace,
                    move |change| match WebPolicy::from_value(change.next().resolved())
                        .map_err(|_| ())
                        .and_then(|policy| watcher.replace_policy(policy).map_err(|_| ()))
                    {
                        Ok(()) => {}
                        Err(()) => watcher.mark_policy_unavailable(),
                    },
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            Ok(())
        }
    }

    Box::new(WebPolicyPlugin)
}

fn domain_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "allow":{"type":"array","items":{"type":"string"},"maxItems":MAX_DOMAIN_RULES},
            "block":{"type":"array","items":{"type":"string"},"maxItems":MAX_DOMAIN_RULES}
        }
    })
}

fn optional_provider(value: Option<&serde_json::Value>) -> Result<Option<String>, WebPolicyError> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if safe_id(value) => Ok(Some(value.clone())),
        Some(_) => Err(WebPolicyError::InvalidProvider),
    }
}

fn string_array(value: Option<&serde_json::Value>) -> Result<Vec<String>, WebPolicyError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or(WebPolicyError::InvalidShape)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(WebPolicyError::InvalidDomain)
        })
        .collect()
}

fn canonical_domains(values: Vec<String>) -> Result<Vec<String>, WebPolicyError> {
    let mut canonical = BTreeSet::new();
    for value in values {
        let domain = canonical_domain(&value)?;
        if !canonical.insert(domain) {
            return Err(WebPolicyError::DuplicateDomain);
        }
    }
    Ok(canonical.into_iter().collect())
}

fn canonical_domain(value: &str) -> Result<String, WebPolicyError> {
    if value.is_empty()
        || value.len() > 253
        || value.trim() != value
        || !value.is_ascii()
        || value.starts_with('.')
        || value.ends_with('.')
    {
        return Err(WebPolicyError::InvalidDomain);
    }
    let value = value.to_ascii_lowercase();
    if value.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    }) {
        return Err(WebPolicyError::InvalidDomain);
    }
    Ok(value)
}

fn domain_matches(host: &str, rule: &str) -> bool {
    host.eq_ignore_ascii_case(rule)
        || host
            .strip_suffix(rule)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn contains_idna_label(host: &str) -> bool {
    host.split('.').any(|label| {
        label
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("xn--"))
    })
}
