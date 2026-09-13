//! Plan-bound Z.ai profiles and their secret-free reports.

use heycode_credentials::{CredentialDescriptor, CredentialSource, CredentialsService};
use heycode_llm::{ProviderDescriptor, ProviderProfile};

use crate::{
    ZAI_GLM_5_3, ZaiCredential, ZaiEndpoint, ZaiPlan, ZaiPlanKind, ZaiProfileError, ZaiProtocol,
};

/// One Z.ai plan's endpoint paired with that same plan's credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiProfile<P: ZaiPlanKind> {
    endpoint: ZaiEndpoint<P>,
    credential: ZaiCredential<P>,
}

impl<P: ZaiPlanKind> ZaiProfile<P> {
    /// Pair an endpoint and a credential that belong to the same plan.
    ///
    /// The plan is part of both parameter types, so a cross-plan pair is a
    /// compile error rather than a runtime check a caller could skip:
    ///
    /// ```
    /// use heycode_provider_zai::{Coding, ZaiCredential, ZaiEndpoint, ZaiProfile, ZaiProtocol};
    ///
    /// let endpoint =
    ///     ZaiEndpoint::<Coding>::documented(ZaiProtocol::OpenAiChatCompletions).unwrap();
    /// let credential = ZaiCredential::<Coding>::default_reference().unwrap();
    /// let profile = ZaiProfile::new(endpoint, credential);
    /// assert_eq!(
    ///     profile.endpoint().base_url(),
    ///     "https://api.z.ai/api/coding/paas/v4"
    /// );
    /// ```
    ///
    /// ```compile_fail
    /// use heycode_provider_zai::{Coding, General, ZaiCredential, ZaiEndpoint, ZaiProfile, ZaiProtocol};
    ///
    /// let endpoint =
    ///     ZaiEndpoint::<Coding>::documented(ZaiProtocol::OpenAiChatCompletions).unwrap();
    /// let credential = ZaiCredential::<General>::default_reference().unwrap();
    /// // The general-plan credential cannot back a Coding Plan endpoint.
    /// let profile = ZaiProfile::new(endpoint, credential);
    /// ```
    #[must_use]
    pub fn new(endpoint: ZaiEndpoint<P>, credential: ZaiCredential<P>) -> Self {
        Self {
            endpoint,
            credential,
        }
    }

    /// This plan's documented endpoint for `protocol` plus its default
    /// credential reference.
    ///
    /// # Errors
    /// Propagates [`ZaiEndpoint::documented`] and
    /// [`ZaiCredential::default_reference`].
    pub fn documented(protocol: ZaiProtocol) -> Result<Self, ZaiProfileError> {
        Ok(Self::new(
            ZaiEndpoint::documented(protocol)?,
            ZaiCredential::default_reference()?,
        ))
    }

    /// Plan this profile serves.
    #[must_use]
    pub fn plan(&self) -> ZaiPlan {
        P::PLAN
    }

    /// Provider registry name and descriptor id.
    #[must_use]
    pub fn registry_name(&self) -> &'static str {
        P::REGISTRY_NAME
    }

    /// Endpoint backing this profile.
    #[must_use]
    pub fn endpoint(&self) -> &ZaiEndpoint<P> {
        &self.endpoint
    }

    /// Credential backing this profile.
    #[must_use]
    pub fn credential(&self) -> &ZaiCredential<P> {
        &self.credential
    }

    /// Published restriction on where this plan's key may be used.
    #[must_use]
    pub fn usage_restriction(&self) -> Option<&'static str> {
        P::USAGE_RESTRICTION
    }

    /// Safe identity/default metadata for setup and routing discovery (B09).
    #[must_use]
    pub fn provider_profile(&self) -> ProviderProfile {
        ProviderProfile {
            registry_name: P::REGISTRY_NAME.to_owned(),
            descriptor: ProviderDescriptor {
                id: P::REGISTRY_NAME.to_owned(),
                display_name: P::DISPLAY_NAME.to_owned(),
                protocols: vec![self.endpoint.protocol().into()],
            },
            default_model: ZAI_GLM_5_3.to_owned(),
            credential_reference: Some(self.credential.reference_name().to_owned()),
        }
    }

    /// Secret-free view of this profile and where its credential comes from.
    ///
    /// This inspects the registry; it never resolves the secret (GOTCHAS #33).
    ///
    /// # Errors
    /// [`ZaiProfileError::Credentials`] carrying the registry's already
    /// redacted text.
    pub fn report(
        &self,
        credentials: &CredentialsService,
    ) -> Result<ZaiProfileReport, ZaiProfileError> {
        let credential = credentials
            .describe(self.credential.query())
            .map_err(|error| ZaiProfileError::Credentials {
                message: error.to_string(),
            })?;
        Ok(ZaiProfileReport {
            plan: P::PLAN,
            registry_name: P::REGISTRY_NAME,
            display_name: P::DISPLAY_NAME,
            protocol: self.endpoint.protocol(),
            endpoint: self.endpoint.base_url(),
            usage_restriction: P::USAGE_RESTRICTION,
            credential,
        })
    }
}

/// What a user may see about one Z.ai plan profile.
///
/// Every field is non-secret by construction: [`CredentialDescriptor`] has no
/// value field, so no rendering of this type can print a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiProfileReport {
    /// Plan in use.
    pub plan: ZaiPlan,
    /// Provider registry name.
    pub registry_name: &'static str,
    /// Human display name.
    pub display_name: &'static str,
    /// Protocol spoken at the endpoint.
    pub protocol: ZaiProtocol,
    /// Base URL requests would go to.
    pub endpoint: &'static str,
    /// Credential reference, provenance and validation state.
    pub credential: CredentialDescriptor,
    /// Published restriction on where this plan's key may be used.
    pub usage_restriction: Option<&'static str>,
}

impl ZaiProfileReport {
    /// One safe line naming the plan, protocol, endpoint and credential
    /// provenance, plus any published usage restriction.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut line = format!(
            "{} ({}) · {} · {} · credential {} ({}) {}",
            self.display_name,
            self.plan.as_str(),
            self.protocol.as_str(),
            self.endpoint,
            self.credential.reference.as_str(),
            self.credential.kind.as_str(),
            self.provenance(),
        );
        if let Some(restriction) = self.usage_restriction {
            line.push_str(" — ");
            line.push_str(restriction);
        }
        line
    }

    fn provenance(&self) -> String {
        match (self.credential.configured, self.credential.source) {
            (true, Some(source)) => format!("from {}", source_label(source)),
            // `describe` always names a source for a configured reference; a
            // descriptor that claims otherwise is reported, not smoothed over.
            (true, None) => "from an unreported source".to_owned(),
            (false, _) => "not configured".to_owned(),
        }
    }
}

/// Both documented plan profiles as secret-free reports, general first.
///
/// Both use OpenAI Chat Completions — the one protocol both plans document —
/// so the two rows are directly comparable and their endpoints and credential
/// references can be seen to differ.
///
/// # Errors
/// Propagates [`ZaiProfile::documented`] and [`ZaiProfile::report`].
pub fn zai_plan_reports(
    credentials: &CredentialsService,
) -> Result<[ZaiProfileReport; 2], ZaiProfileError> {
    let general = ZaiProfile::<crate::General>::documented(ZaiProtocol::OpenAiChatCompletions)?
        .report(credentials)?;
    let coding = ZaiProfile::<crate::Coding>::documented(ZaiProtocol::OpenAiChatCompletions)?
        .report(credentials)?;
    Ok([general, coding])
}

fn source_label(source: CredentialSource) -> &'static str {
    match source {
        CredentialSource::Environment => "the process environment",
        CredentialSource::Keychain => "the OS keychain",
        CredentialSource::File => "the heycode credential file",
        CredentialSource::Command => "a credential command",
        CredentialSource::AmbientRuntime => "an ambient runtime chain",
        CredentialSource::SubscriptionRuntime => "a subscription runtime",
        // `CredentialSource` is `#[non_exhaustive]`; a source this crate has
        // not been taught is named as unrecognized rather than mislabelled.
        _ => "an unrecognized credential source",
    }
}
