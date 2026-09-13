//! What an exported OTLP resource is allowed to say about this machine.
//!
//! An OTEL resource is where a real deployment leaks. The SDK convention is to
//! fill it from the environment — `OTEL_RESOURCE_ATTRIBUTES` is free-form
//! `key=value` text, and the standard attribute set includes
//! `process.command_line`, which on this product routinely contains
//! `--api-key <secret>`. Both are open channels into a document that leaves the
//! machine.
//!
//! Neither is admitted here.
//!
//! [`OtlpAttributeKey`] is a closed set, and what it does **not** contain is the
//! contract: no `host.name`, no `host.id`, no `process.command_line`, no
//! `process.owner`, no `service.instance.id`, no `user.*`. Those are the
//! attributes an OTEL default resource detector adds for you, and each one is
//! either the machine's identity or a place a credential rides along. A build
//! that wants one has to add a variant here, which is a deliberate act with a
//! review attached rather than a detector switched on.
//!
//! [`OtlpResource::extend_from_spec`] is the concession to reality: operators
//! do set `OTEL_RESOURCE_ATTRIBUTES`, and silently ignoring it would be a
//! product that lies about its own configuration. It parses the spec and
//! reports, per closed reason, **how many** pairs it refused — never which, and
//! never the text. A refusal report that quoted the offending pair would be the
//! leak the parser exists to prevent.

use serde::{Serialize, Serializer};

use crate::{Label, TelemetryFault};

/// Longest `OTEL_RESOURCE_ATTRIBUTES`-style spec this parser will read.
///
/// Past this the whole spec is refused rather than truncated. A truncated parse
/// admits a prefix and leaves the reader believing the rest was considered,
/// which is the partial-record failure Q08 refuses for the same reason.
pub const ATTRIBUTE_SPEC_MAX_BYTES: usize = 4_096;

/// Most resource attributes one export can carry.
///
/// A consequence of the closed key set rather than a limit anyone enforces: an
/// attribute may be set once, so the bound is the type. A cap that cannot be
/// reached is protection in appearance only (GOTCHAS #152).
pub const MAX_RESOURCE_ATTRIBUTES: usize = OtlpAttributeKey::ALL.len();

/// Which resource attribute a value describes, from a closed set.
///
/// Every variant is either a constant this build supplies or a short registry
/// identifier an operator names their deployment with. There is no variant for
/// a command line, a hostname, an instance id or a user, and that absence is
/// the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum OtlpAttributeKey {
    /// `service.name` — what this deployment calls itself.
    ServiceName,
    /// `service.version` — the build's own version.
    ServiceVersion,
    /// `service.namespace` — an operator's grouping for related services.
    ServiceNamespace,
    /// `deployment.environment` — `production`, `staging` and the like.
    DeploymentEnvironment,
    /// `os.type` — the platform family, as a closed identifier.
    OsType,
    /// `telemetry.sdk.name` — supplied by this crate, never by a caller.
    TelemetrySdkName,
    /// `telemetry.sdk.language` — supplied by this crate.
    TelemetrySdkLanguage,
    /// `telemetry.sdk.version` — supplied by this crate.
    TelemetrySdkVersion,
}

impl OtlpAttributeKey {
    /// Every key, in stable order.
    pub const ALL: [Self; 8] = [
        Self::ServiceName,
        Self::ServiceVersion,
        Self::ServiceNamespace,
        Self::DeploymentEnvironment,
        Self::OsType,
        Self::TelemetrySdkName,
        Self::TelemetrySdkLanguage,
        Self::TelemetrySdkVersion,
    ];

    /// The OTLP attribute name, as a compile-time constant.
    ///
    /// Returning `&'static str` rather than `String` is the point: the key side
    /// of an exported payload has nowhere for caller text to be.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServiceName => "service.name",
            Self::ServiceVersion => "service.version",
            Self::ServiceNamespace => "service.namespace",
            Self::DeploymentEnvironment => "deployment.environment",
            Self::OsType => "os.type",
            Self::TelemetrySdkName => "telemetry.sdk.name",
            Self::TelemetrySdkLanguage => "telemetry.sdk.language",
            Self::TelemetrySdkVersion => "telemetry.sdk.version",
        }
    }

    /// The key an operator's spec would have to name to reach this attribute.
    ///
    /// Only keys this build supplies from the outside are addressable; the
    /// `telemetry.sdk.*` trio describes the exporter itself, so a spec that
    /// names one is refused as unknown rather than allowed to rewrite what this
    /// build says it is.
    #[must_use]
    pub const fn operator_settable(self) -> bool {
        matches!(
            self,
            Self::ServiceName
                | Self::ServiceVersion
                | Self::ServiceNamespace
                | Self::DeploymentEnvironment
                | Self::OsType
        )
    }

    fn from_spec_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.operator_settable() && candidate.as_str() == key)
    }
}

impl std::fmt::Display for OtlpAttributeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for OtlpAttributeKey {
    /// Serializes as the OTLP attribute name, not the variant name, so a
    /// diagnostic snapshot reads the same as the wire.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Why one pair in an attribute spec was not admitted, from a closed set.
///
/// A reason carries no text, for the same reason [`TelemetryFault`] does not:
/// the refused pair is exactly the material a refusal report must not echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AttributeRefusal {
    /// The whole spec was over [`ATTRIBUTE_SPEC_MAX_BYTES`]; nothing was read.
    OversizeSpec,
    /// The pair had no `=`.
    MalformedPair,
    /// The key is not an operator-settable [`OtlpAttributeKey`].
    UnknownKey,
    /// The value is not a bounded registry identifier.
    MalformedValue,
    /// The value matches a recognized credential format.
    CredentialMaterial,
    /// The key was already set, here or by an earlier pair.
    DuplicateKey,
}

impl AttributeRefusal {
    /// Every reason, in stable order. The counter array is sized from this.
    pub const ALL: [Self; 6] = [
        Self::OversizeSpec,
        Self::MalformedPair,
        Self::UnknownKey,
        Self::MalformedValue,
        Self::CredentialMaterial,
        Self::DuplicateKey,
    ];

    /// How many reasons exist.
    pub const COUNT: usize = Self::ALL.len();

    /// Position of this reason in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::OversizeSpec => 0,
            Self::MalformedPair => 1,
            Self::UnknownKey => 2,
            Self::MalformedValue => 3,
            Self::CredentialMaterial => 4,
            Self::DuplicateKey => 5,
        }
    }

    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OversizeSpec => "oversize_spec",
            Self::MalformedPair => "malformed_pair",
            Self::UnknownKey => "unknown_key",
            Self::MalformedValue => "malformed_value",
            Self::CredentialMaterial => "credential_material",
            Self::DuplicateKey => "duplicate_key",
        }
    }
}

impl std::fmt::Display for AttributeRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// How many pairs an attribute spec lost, and to which reason.
///
/// Counts only. This is what a doctor check or a `/telemetry` panel may say
/// about a misconfigured `OTEL_RESOURCE_ATTRIBUTES`: that four pairs were
/// refused as credential material, never which four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttributeSpecRefusals {
    counts: [usize; AttributeRefusal::COUNT],
}

impl AttributeSpecRefusals {
    fn refuse(&mut self, reason: AttributeRefusal) {
        self.counts[reason.index()] = self.counts[reason.index()].saturating_add(1);
    }

    /// How many pairs were refused for `reason`.
    #[must_use]
    pub const fn count(&self, reason: AttributeRefusal) -> usize {
        self.counts[reason.index()]
    }

    /// Every reason with its count, in [`AttributeRefusal::ALL`] order.
    #[must_use]
    pub fn counts(&self) -> Vec<(AttributeRefusal, usize)> {
        AttributeRefusal::ALL
            .into_iter()
            .map(|reason| (reason, self.count(reason)))
            .collect()
    }

    /// Total pairs refused.
    #[must_use]
    pub fn total(&self) -> usize {
        AttributeRefusal::ALL
            .into_iter()
            .fold(0, |total, reason| total.saturating_add(self.count(reason)))
    }

    /// Whether every pair in the spec was admitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// The resource an export attributes its events to.
///
/// Built, never parsed: there is no `Deserialize`, so no configuration file can
/// restore one past its constructors (GOTCHAS #161). Every value inside is a
/// [`Label`] and every key a closed variant, which is what makes the serialized
/// resource carry no free text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct OtlpResource {
    attributes: Vec<(OtlpAttributeKey, Label)>,
}

impl OtlpResource {
    /// The resource for a build calling itself `service_name`.
    ///
    /// Seeds the `telemetry.sdk.*` trio from this crate's own identity, so an
    /// exported document always says which exporter produced it. Those three
    /// are set here and are not operator-settable, so no spec can make this
    /// build claim to be a different SDK.
    #[must_use]
    pub fn new(service_name: Label) -> Self {
        let mut attributes = Vec::with_capacity(MAX_RESOURCE_ATTRIBUTES);
        attributes.push((OtlpAttributeKey::ServiceName, service_name));
        for (key, value) in [
            (OtlpAttributeKey::TelemetrySdkName, SDK_NAME),
            (OtlpAttributeKey::TelemetrySdkLanguage, SDK_LANGUAGE),
            (OtlpAttributeKey::TelemetrySdkVersion, SDK_VERSION),
        ] {
            if let Ok(label) = Label::new(value) {
                attributes.push((key, label));
            }
        }
        Self { attributes }
    }

    /// Attach one attribute.
    ///
    /// # Errors
    /// [`TelemetryFault::DuplicateAttribute`] when the key is already set —
    /// silently overwriting would let a later call change what an earlier one
    /// recorded, and would let a spec rewrite this build's own SDK identity.
    pub fn with_attribute(
        mut self,
        key: OtlpAttributeKey,
        value: Label,
    ) -> Result<Self, TelemetryFault> {
        self.push_attribute(key, value)?;
        Ok(self)
    }

    /// The one place an attribute is admitted.
    ///
    /// [`Self::with_attribute`] and [`Self::extend_from_spec`] both route
    /// through here rather than each testing for a duplicate, so there is one
    /// rule about repeats and no second copy of it to drift (GOTCHAS #160).
    fn push_attribute(
        &mut self,
        key: OtlpAttributeKey,
        value: Label,
    ) -> Result<(), TelemetryFault> {
        if self.attributes.iter().any(|(existing, _)| *existing == key) {
            return Err(TelemetryFault::DuplicateAttribute);
        }
        self.attributes.push((key, value));
        Ok(())
    }

    /// Read an `OTEL_RESOURCE_ATTRIBUTES`-style `k=v,k=v` spec.
    ///
    /// Admits only operator-settable keys whose values pass [`Label::new`], and
    /// reports the rest as counts by reason. Infallible on purpose: a spec that
    /// is entirely garbage produces a resource with no operator attributes and
    /// a refusal report, rather than a failure that would take telemetry down
    /// over a typo in an environment variable.
    #[must_use]
    pub fn extend_from_spec(mut self, spec: &str) -> (Self, AttributeSpecRefusals) {
        let mut refusals = AttributeSpecRefusals::default();
        if spec.len() > ATTRIBUTE_SPEC_MAX_BYTES {
            refusals.refuse(AttributeRefusal::OversizeSpec);
            return (self, refusals);
        }
        for pair in spec.split(',') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let Some((key, value)) = pair.split_once('=') else {
                refusals.refuse(AttributeRefusal::MalformedPair);
                continue;
            };
            let Some(key) = OtlpAttributeKey::from_spec_key(key.trim()) else {
                refusals.refuse(AttributeRefusal::UnknownKey);
                continue;
            };
            let label = match Label::new(value.trim()) {
                Ok(label) => label,
                Err(TelemetryFault::CredentialMaterial) => {
                    refusals.refuse(AttributeRefusal::CredentialMaterial);
                    continue;
                }
                Err(_) => {
                    refusals.refuse(AttributeRefusal::MalformedValue);
                    continue;
                }
            };
            if self.push_attribute(key, label).is_err() {
                refusals.refuse(AttributeRefusal::DuplicateKey);
            }
        }
        (self, refusals)
    }

    /// Attached attributes, in the order they were supplied.
    #[must_use]
    pub fn attributes(&self) -> &[(OtlpAttributeKey, Label)] {
        &self.attributes
    }

    /// The value on `key`, when one was attached.
    #[must_use]
    pub fn attribute(&self, key: OtlpAttributeKey) -> Option<&Label> {
        self.attributes
            .iter()
            .find(|(existing, _)| *existing == key)
            .map(|(_, value)| value)
    }
}

/// What this exporter calls itself in `telemetry.sdk.name`.
const SDK_NAME: &str = "heycode-telemetry";
/// The implementation language, per OTLP semantic conventions.
const SDK_LANGUAGE: &str = "rust";
/// This crate's version, so a reader knows which build produced a document.
const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-ant-api03-0123456789abcdef";

    fn resource() -> OtlpResource {
        OtlpResource::new(Label::new("heycode").unwrap())
    }

    #[test]
    fn a_new_resource_states_which_exporter_produced_it() {
        let resource = resource();
        assert_eq!(
            resource
                .attribute(OtlpAttributeKey::TelemetrySdkName)
                .map(Label::as_str),
            Some(SDK_NAME)
        );
        assert_eq!(
            resource
                .attribute(OtlpAttributeKey::TelemetrySdkLanguage)
                .map(Label::as_str),
            Some("rust")
        );
        assert_eq!(
            resource
                .attribute(OtlpAttributeKey::TelemetrySdkVersion)
                .map(Label::as_str),
            Some(SDK_VERSION)
        );
        assert_eq!(
            resource
                .attribute(OtlpAttributeKey::ServiceName)
                .map(Label::as_str),
            Some("heycode")
        );
    }

    #[test]
    fn every_sdk_constant_is_a_constructible_label() {
        // The seeding loop drops a value `Label::new` refuses rather than
        // panicking, so a bad constant would silently omit an attribute. This
        // is the test that turns that into a failure.
        for value in [SDK_NAME, SDK_LANGUAGE, SDK_VERSION] {
            assert!(Label::new(value).is_ok(), "{value} is not a valid label");
        }
    }

    #[test]
    fn the_closed_key_set_names_nothing_that_identifies_the_machine_or_its_argv() {
        let published: Vec<&str> = OtlpAttributeKey::ALL
            .iter()
            .map(|key| key.as_str())
            .collect();
        for forbidden in [
            "host.name",
            "host.id",
            "host.arch",
            "process.command_line",
            "process.command_args",
            "process.owner",
            "process.executable.path",
            "service.instance.id",
            "user.name",
            "user.id",
        ] {
            assert!(
                !published.contains(&forbidden),
                "{forbidden} is what an OTEL default resource detector adds; \
                 its absence here is the contract"
            );
        }
    }

    #[test]
    fn attribute_keys_are_unique_and_the_sdk_trio_is_not_operator_settable() {
        let mut keys: Vec<&str> = OtlpAttributeKey::ALL
            .iter()
            .map(|key| key.as_str())
            .collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "duplicate attribute key identifier");

        for key in OtlpAttributeKey::ALL {
            let settable = key.operator_settable();
            assert_eq!(
                OtlpAttributeKey::from_spec_key(key.as_str()).is_some(),
                settable,
                "{key} must be reachable from a spec exactly when it is settable"
            );
            if key.as_str().starts_with("telemetry.sdk.") {
                assert!(!settable, "{key} describes this build, not a deployment");
            }
        }
    }

    #[test]
    fn a_resource_refuses_a_repeated_attribute_rather_than_overwriting_it() {
        let resource = resource();
        assert_eq!(
            resource
                .clone()
                .with_attribute(OtlpAttributeKey::ServiceName, Label::new("other").unwrap()),
            Err(TelemetryFault::DuplicateAttribute)
        );
        assert_eq!(
            resource
                .attribute(OtlpAttributeKey::ServiceName)
                .map(Label::as_str),
            Some("heycode")
        );
    }

    #[test]
    fn a_resource_is_bounded_at_one_value_per_key_by_the_closed_key_set() {
        let mut resource = resource();
        for key in OtlpAttributeKey::ALL {
            if resource.attribute(key).is_none() {
                resource = resource
                    .with_attribute(key, Label::new(key.as_str()).unwrap())
                    .unwrap();
            }
        }
        assert_eq!(resource.attributes().len(), MAX_RESOURCE_ATTRIBUTES);
        for key in OtlpAttributeKey::ALL {
            assert_eq!(
                resource
                    .clone()
                    .with_attribute(key, Label::new("more").unwrap()),
                Err(TelemetryFault::DuplicateAttribute),
                "{key} must not be attachable twice"
            );
        }
    }

    #[test]
    fn a_spec_admits_a_settable_key_and_counts_nothing() {
        let (resource, refusals) =
            resource().extend_from_spec("deployment.environment=staging, service.namespace=team-a");
        assert!(refusals.is_empty(), "{refusals:?}");
        assert_eq!(
            resource
                .attribute(OtlpAttributeKey::DeploymentEnvironment)
                .map(Label::as_str),
            Some("staging")
        );
        assert_eq!(
            resource
                .attribute(OtlpAttributeKey::ServiceNamespace)
                .map(Label::as_str),
            Some("team-a")
        );
    }

    #[test]
    fn a_spec_refuses_an_unknown_key_rather_than_carrying_it() {
        let (resource, refusals) = resource().extend_from_spec("api_key=abcdef,host.name=laptop");
        assert_eq!(refusals.count(AttributeRefusal::UnknownKey), 2);
        assert_eq!(refusals.total(), 2);
        assert_eq!(resource.attributes().len(), 4, "only the seeded four");
    }

    #[test]
    fn a_spec_refuses_a_credential_value_and_says_only_how_many() {
        let (resource, refusals) =
            resource().extend_from_spec(&format!("service.namespace={SECRET}"));
        assert_eq!(refusals.count(AttributeRefusal::CredentialMaterial), 1);
        assert!(
            resource
                .attribute(OtlpAttributeKey::ServiceNamespace)
                .is_none()
        );
        let rendered = format!("{refusals:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(!rendered.contains("sk-"), "{rendered}");
    }

    #[test]
    fn a_spec_separates_a_malformed_value_from_a_credential_one() {
        let (_, refusals) = resource().extend_from_spec("service.namespace=two words,os.type=fine");
        assert_eq!(refusals.count(AttributeRefusal::MalformedValue), 1);
        assert_eq!(refusals.count(AttributeRefusal::CredentialMaterial), 0);
    }

    #[test]
    fn a_spec_counts_a_missing_equals_and_a_repeated_key() {
        let (_, refusals) =
            resource().extend_from_spec("justakey,os.type=linux,os.type=darwin,service.name=x");
        assert_eq!(refusals.count(AttributeRefusal::MalformedPair), 1);
        assert_eq!(
            refusals.count(AttributeRefusal::DuplicateKey),
            2,
            "the repeated os.type and the already-seeded service.name"
        );
    }

    #[test]
    fn a_spec_that_names_an_sdk_key_is_refused_as_unknown() {
        let (resource, refusals) = resource()
            .extend_from_spec("telemetry.sdk.name=not-heycode,telemetry.sdk.version=99.0");
        assert_eq!(refusals.count(AttributeRefusal::UnknownKey), 2);
        assert_eq!(
            resource
                .attribute(OtlpAttributeKey::TelemetrySdkName)
                .map(Label::as_str),
            Some(SDK_NAME),
            "a spec must not be able to make this build claim a different SDK"
        );
    }

    #[test]
    fn an_oversize_spec_is_refused_whole_rather_than_truncated() {
        let mut spec = String::from("os.type=linux,");
        while spec.len() <= ATTRIBUTE_SPEC_MAX_BYTES {
            spec.push_str("service.namespace=x,");
        }
        let (resource, refusals) = resource().extend_from_spec(&spec);
        assert_eq!(refusals.count(AttributeRefusal::OversizeSpec), 1);
        assert_eq!(refusals.total(), 1, "nothing else was even read");
        assert!(
            resource.attribute(OtlpAttributeKey::OsType).is_none(),
            "a prefix must not be admitted from a spec that was refused"
        );
    }

    #[test]
    fn refusal_counts_report_every_reason_in_stable_order() {
        let refusals = AttributeSpecRefusals::default();
        assert!(refusals.is_empty());
        let reported: Vec<AttributeRefusal> =
            refusals.counts().into_iter().map(|(r, _)| r).collect();
        assert_eq!(reported, AttributeRefusal::ALL.to_vec());
        for (position, reason) in AttributeRefusal::ALL.into_iter().enumerate() {
            assert_eq!(reason.index(), position, "{reason} is misindexed");
        }
        assert_eq!(AttributeRefusal::COUNT, AttributeRefusal::ALL.len());
    }
}
