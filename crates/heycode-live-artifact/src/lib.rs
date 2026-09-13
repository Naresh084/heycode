//! Q08 live-test artifacts: what a live route exercise is allowed to record.
//!
//! A live test talks to a real provider with a real credential, and its output
//! outlives the run — it lands in CI logs, in a dashboard, in a repository. So
//! the schema is built around one asymmetry: metadata and outcome are always
//! safe to keep, and everything else is withheld until someone says otherwise.
//!
//! Two independent gates, deliberately not one. Opting in to **content** is not
//! opting in to **credentials**: captured content is screened regardless, so a
//! response body that happens to echo an API key is refused even from a run
//! that asked for bodies. A single "verbose" flag would have collapsed those
//! into each other, which is exactly the mistake this row exists to prevent.

use std::time::Duration;

use heycode_settings::WireExposureFault;
use serde::{Deserialize, Serialize};

/// Schema version of the artifact format.
///
/// Bumped whenever a reader could misinterpret an older artifact. A reader that
/// does not recognize the version must refuse the file rather than guess.
pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;

/// Why an artifact could not be recorded as asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArtifactFault {
    /// Text offered for capture contains recognizable credential material.
    CredentialMaterial,
    /// A metadata key names credential material.
    CredentialShapedKey,
    /// A route field was empty or carried a control character.
    MalformedRoute,
}

impl std::fmt::Display for ArtifactFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CredentialMaterial => "the text contains recognizable credential material",
            Self::CredentialShapedKey => "the metadata key names credential material",
            Self::MalformedRoute => "the route is empty or contains a control character",
        })
    }
}

impl std::error::Error for ArtifactFault {}

impl From<WireExposureFault> for ArtifactFault {
    fn from(_: WireExposureFault) -> Self {
        // The settings fault is deliberately dropped rather than wrapped: it is
        // already a closed set, but re-exporting it here would let a future
        // variant that carries detail reach an artifact.
        Self::CredentialMaterial
    }
}

/// Which live route an exercise ran against.
///
/// Provider and model only. No endpoint URL: a URL is the one metadata field
/// that routinely carries a token in its query string, and no screen is worth
/// trusting when withholding costs nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteId {
    provider: String,
    model: String,
}

impl RouteId {
    /// Validate and build a route identity.
    ///
    /// # Errors
    /// [`ArtifactFault::MalformedRoute`] for an empty or control-bearing field.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, ArtifactFault> {
        let provider = provider.into();
        let model = model.into();
        let sane = |value: &str| {
            !value.trim().is_empty() && !value.chars().any(char::is_control) && value.len() <= 200
        };
        if !sane(&provider) || !sane(&model) {
            return Err(ArtifactFault::MalformedRoute);
        }
        Ok(Self { provider, model })
    }

    /// Provider name.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Model name.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// How a live exercise ended.
///
/// A closed set, so a failure can never carry the offending text into the
/// artifact — the same rule S15 applies to wire-exposure faults. "Why did it
/// fail" is answered by the class; the message stays in the runner's own logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureClass {
    /// The credential was rejected.
    Unauthorized,
    /// The provider refused for quota or rate reasons.
    RateLimited,
    /// The host could not be reached.
    HostUnreachable,
    /// A response did not match the protocol the adapter expects.
    ProtocolMismatch,
    /// The exercise exceeded its time budget.
    Timeout,
    /// The route answered, but an assertion about the answer failed.
    AssertionFailed,
    /// Classification was not possible. Never treat this as any other class.
    Unclassified,
}

/// Why an exercise did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SkipReason {
    /// No credential was configured for this route.
    NoCredential,
    /// The route is not enabled in this environment.
    NotEnabled,
    /// A prerequisite exercise failed.
    PrerequisiteFailed,
}

/// What an exercise concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
#[non_exhaustive]
pub enum LiveOutcome {
    /// The route answered and every assertion held.
    Passed,
    /// The route was reached or attempted and the exercise failed.
    Failed {
        /// Closed classification; never free text.
        class: FailureClass,
    },
    /// The exercise did not run.
    Skipped {
        /// Closed reason; never free text.
        reason: SkipReason,
    },
}

impl LiveOutcome {
    /// True only for a clean pass.
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, Self::Passed)
    }
}

/// Text that has passed the credential screen.
///
/// There is no way to build one except through [`ScreenedText::screen`] —
/// **including on the read path**. `try_from` routes deserialization through
/// the same constructor, so a hand-authored artifact cannot carry a credential
/// past the screen. This was `#[serde(transparent)]` at first, and it could:
/// a constructor invariant that deserialization bypasses is not an invariant,
/// it is a convention with a hole in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct ScreenedText(String);

impl From<ScreenedText> for String {
    fn from(text: ScreenedText) -> Self {
        text.0
    }
}

impl TryFrom<String> for ScreenedText {
    type Error = ArtifactFault;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::screen(text)
    }
}

impl ScreenedText {
    /// Screen `text`, yielding a value that may be recorded.
    ///
    /// # Errors
    /// [`ArtifactFault::CredentialMaterial`] when the text contains anything
    /// the shared settings screen recognizes as credential material.
    pub fn screen(text: impl Into<String>) -> Result<Self, ArtifactFault> {
        let text = text.into();
        heycode_settings::screen_text_for_credentials(&text)?;
        Ok(Self(text))
    }

    /// The screened text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether this run may record request/response content at all.
///
/// Not a `bool`. A bool has a default, and the default that leaks content is
/// the failure this row exists to prevent; a caller must name the decision, and
/// the name appears in the artifact so a reader knows what they are looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentPolicy {
    /// Content is never recorded. The default for every environment, and the
    /// `#[default]` lives on the variant so a reader cannot miss which way the
    /// omission falls.
    #[default]
    Withhold,
    /// Content may be recorded, subject to the credential screen it must still
    /// pass. Opting in to content is not opting in to credentials.
    RecordOptedIn,
}

/// Request/response content, if this run was allowed to keep any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "capture")]
#[non_exhaustive]
pub enum ContentCapture {
    /// No content was recorded, and the artifact says why.
    Withheld {
        /// The policy in force when the exercise ran.
        policy: ContentPolicy,
    },
    /// Screened content, recorded under an explicit opt-in.
    Recorded {
        /// Content that passed the credential screen.
        body: ScreenedText,
    },
}

impl ContentCapture {
    /// The recorded body, if any.
    #[must_use]
    pub fn body(&self) -> Option<&str> {
        match self {
            Self::Withheld { .. } => None,
            Self::Recorded { body } => Some(body.as_str()),
        }
    }
}

/// One recorded live-route exercise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveArtifact {
    schema_version: u32,
    route: RouteId,
    recorded_at_unix_ms: u64,
    outcome: LiveOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
    content: ContentCapture,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    notes: Vec<ScreenedText>,
}

/// Builds a [`LiveArtifact`] under one content policy.
///
/// The policy is supplied when the recorder is created, not when content is
/// offered, so no call site can quietly widen it mid-run.
#[derive(Debug, Clone)]
pub struct ArtifactRecorder {
    policy: ContentPolicy,
}

impl ArtifactRecorder {
    /// A recorder that never keeps content.
    #[must_use]
    pub const fn withholding() -> Self {
        Self {
            policy: ContentPolicy::Withhold,
        }
    }

    /// A recorder allowed to keep screened content.
    ///
    /// The caller is asserting that an operator opted in for this environment.
    #[must_use]
    pub const fn opted_in() -> Self {
        Self {
            policy: ContentPolicy::RecordOptedIn,
        }
    }

    /// The policy in force.
    #[must_use]
    pub const fn policy(&self) -> ContentPolicy {
        self.policy
    }

    /// Record one exercise.
    ///
    /// `body` is offered, never imposed: under [`ContentPolicy::Withhold`] it is
    /// dropped without being screened, because the cheapest way to not leak text
    /// is to never look at it.
    ///
    /// # Errors
    /// [`ArtifactFault::CredentialMaterial`] when an opted-in body or any note
    /// contains recognizable credential material. The artifact is not produced:
    /// a partial record that silently dropped the offending field would leave a
    /// reader believing they saw everything.
    pub fn record(
        &self,
        route: RouteId,
        outcome: LiveOutcome,
        recorded_at_unix_ms: u64,
        latency: Option<Duration>,
        body: Option<&str>,
        notes: &[&str],
    ) -> Result<LiveArtifact, ArtifactFault> {
        let content = match (self.policy, body) {
            (ContentPolicy::RecordOptedIn, Some(body)) => ContentCapture::Recorded {
                body: ScreenedText::screen(body)?,
            },
            (policy, _) => ContentCapture::Withheld { policy },
        };
        let notes = notes
            .iter()
            .map(|note| ScreenedText::screen(*note))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LiveArtifact {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            route,
            recorded_at_unix_ms,
            outcome,
            latency_ms: latency
                .map(|latency| u64::try_from(latency.as_millis()).unwrap_or(u64::MAX)),
            content,
            notes,
        })
    }
}

impl LiveArtifact {
    /// Schema version this artifact was written at.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The route exercised.
    #[must_use]
    pub const fn route(&self) -> &RouteId {
        &self.route
    }

    /// Artifact recording instant in Unix milliseconds.
    #[must_use]
    pub const fn recorded_at_unix_ms(&self) -> u64 {
        self.recorded_at_unix_ms
    }

    /// Whether this artifact is no older than `maximum_age` at `now_unix_ms`.
    ///
    /// A future-dated artifact is not fresh: accepting it would let clock skew
    /// or a hand-authored timestamp satisfy a continuous-evidence gate forever.
    #[must_use]
    pub fn is_fresh_at(&self, now_unix_ms: u64, maximum_age: Duration) -> bool {
        let maximum_age_ms = u64::try_from(maximum_age.as_millis()).unwrap_or(u64::MAX);
        now_unix_ms
            .checked_sub(self.recorded_at_unix_ms)
            .is_some_and(|age| age <= maximum_age_ms)
    }

    /// What the exercise concluded.
    #[must_use]
    pub const fn outcome(&self) -> LiveOutcome {
        self.outcome
    }

    /// Wall-clock duration, when the runner measured one.
    #[must_use]
    pub const fn latency_ms(&self) -> Option<u64> {
        self.latency_ms
    }

    /// Content capture, including the policy when nothing was kept.
    #[must_use]
    pub const fn content(&self) -> &ContentCapture {
        &self.content
    }

    /// Screened free-text notes.
    #[must_use]
    pub fn notes(&self) -> &[ScreenedText] {
        &self.notes
    }

    /// Read an artifact, refusing any schema version this build cannot read.
    ///
    /// # Errors
    /// A parse failure, or a version mismatch reported as a parse failure so no
    /// caller can act on a field it does not understand.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let artifact: Self = serde_json::from_str(json)?;
        if artifact.schema_version != ARTIFACT_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unreadable artifact schema version {}; this build reads {ARTIFACT_SCHEMA_VERSION}",
                artifact.schema_version
            )));
        }
        Ok(artifact)
    }
}
