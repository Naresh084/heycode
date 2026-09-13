//! Verified release-attestation boundary.

use crate::channel::UpdateApproval;
use crate::error::{invalid_field, mismatch};
use crate::{
    ArtifactDigest, ArtifactMismatch, ReleaseManifest, ReleasePlatform, ReleaseVersion,
    SignatureScheme, UpdateError,
};

pub(crate) const MAX_SIGNATURE_BUNDLE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_RELEASE_ARTIFACT_BYTES: usize = 512 * 1024 * 1024;
const GITHUB_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
const MAX_REPOSITORY_BYTES: usize = 256;
const MAX_WORKFLOW_BYTES: usize = 512;
const MAX_SOURCE_REF_BYTES: usize = 256;

/// Which exact subject a signature verifier was asked to authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureSubject {
    /// The release manifest document.
    Manifest,
    /// One platform artifact named by that manifest.
    Artifact,
}

impl std::fmt::Display for SignatureSubject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Manifest => "manifest",
            Self::Artifact => "artifact",
        })
    }
}

/// Body-free result classes from an external signature verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignatureVerificationFault {
    /// The bundle, signature, subject, or expected identity did not verify.
    Invalid,
    /// Verification could not complete because its trust service was unavailable.
    Unavailable,
    /// The verifier cannot process the required signature scheme.
    Unsupported,
}

impl std::fmt::Display for SignatureVerificationFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
        })
    }
}

/// Locally configured signer identity for GitHub artifact attestations.
///
/// The repository and workflow are operator/build-distribution policy, not
/// values trusted from the downloaded manifest. There is deliberately no key
/// or secret field: GitHub/Sigstore verification uses a short-lived OIDC
/// identity and a public trust root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTrustPolicy {
    repository: String,
    workflow: String,
    source_ref: Option<String>,
}

impl ReleaseTrustPolicy {
    /// Configure an exact GitHub repository and workflow signer.
    ///
    /// # Errors
    /// Unsafe, ambiguous, or unbounded repository/workflow identities are
    /// rejected before a verifier can receive them.
    pub fn github(repository: &str, workflow: &str) -> Result<Self, UpdateError> {
        if !valid_repository(repository) {
            return Err(invalid_field(
                "trust.repository",
                "must be one bounded owner/repository identity",
            ));
        }
        if !valid_workflow(workflow) {
            return Err(invalid_field(
                "trust.workflow",
                "must be one normalized workflow path under .github/workflows",
            ));
        }
        Ok(Self {
            repository: repository.to_owned(),
            workflow: workflow.to_owned(),
            source_ref: None,
        })
    }

    /// Configure an exact GitHub repository, signer workflow, and release tag.
    ///
    /// # Errors
    /// Repository/workflow validation from [`Self::github`] applies, and the
    /// ref must be one bounded normalized `refs/tags/...` identity.
    pub fn github_at_ref(
        repository: &str,
        workflow: &str,
        source_ref: &str,
    ) -> Result<Self, UpdateError> {
        if !valid_source_ref(source_ref) {
            return Err(invalid_field(
                "trust.source_ref",
                "must be one normalized release tag ref",
            ));
        }
        let mut trust = Self::github(repository, workflow)?;
        trust.source_ref = Some(source_ref.to_owned());
        Ok(trust)
    }

    /// Expected GitHub `owner/repository` identity.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Expected repository-relative signer workflow.
    #[must_use]
    pub fn workflow(&self) -> &str {
        &self.workflow
    }

    /// Required GitHub Actions OIDC issuer.
    #[must_use]
    pub const fn issuer(&self) -> &'static str {
        GITHUB_OIDC_ISSUER
    }

    /// Exact source tag required from the attestation certificate, when set.
    #[must_use]
    pub fn source_ref(&self) -> Option<&str> {
        self.source_ref.as_deref()
    }
}

/// Exact, non-loggable inputs to a release signature verifier.
///
/// This type deliberately has no `Debug`: subject and bundle bytes may contain
/// executable or remotely supplied data.
pub struct SignatureVerificationRequest<'a> {
    subject: SignatureSubject,
    subject_bytes: &'a [u8],
    bundle: &'a [u8],
    trust: &'a ReleaseTrustPolicy,
}

impl SignatureVerificationRequest<'_> {
    /// Subject class being authenticated.
    #[must_use]
    pub const fn subject(&self) -> SignatureSubject {
        self.subject
    }

    /// Exact bytes whose signature must verify.
    #[must_use]
    pub const fn subject_bytes(&self) -> &[u8] {
        self.subject_bytes
    }

    /// Exact detached Sigstore bundle bytes.
    #[must_use]
    pub const fn bundle(&self) -> &[u8] {
        self.bundle
    }

    /// Exact configured GitHub repository identity.
    #[must_use]
    pub fn expected_repository(&self) -> &str {
        self.trust.repository()
    }

    /// Exact configured signer workflow path.
    #[must_use]
    pub fn expected_workflow(&self) -> &str {
        self.trust.workflow()
    }

    /// Exact configured OIDC issuer.
    #[must_use]
    pub const fn expected_issuer(&self) -> &str {
        self.trust.issuer()
    }

    /// Exact configured source tag, when the host requires one.
    #[must_use]
    pub fn expected_source_ref(&self) -> Option<&str> {
        self.trust.source_ref()
    }
}

/// Host-supplied cryptographic verifier for release attestations.
///
/// Implementations are expected to verify the Sigstore bundle, transparency
/// evidence, exact subject bytes, repository, workflow, and OIDC issuer. The
/// heycode install crate owns policy and mutation ordering; it intentionally owns
/// no production signing secret and does not implement cryptography itself.
pub trait ReleaseSignatureVerifier: Send + Sync {
    /// Verify one exact subject and return only a closed failure class.
    ///
    /// # Errors
    /// [`SignatureVerificationFault`] identifies invalid, unavailable, or
    /// unsupported verification without exposing verifier output.
    fn verify(
        &self,
        request: SignatureVerificationRequest<'_>,
    ) -> Result<(), SignatureVerificationFault>;
}

/// Proof metadata minted only after a verifier accepted exact bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSignature {
    scheme: SignatureScheme,
    bundle_digest: ArtifactDigest,
    repository: String,
    workflow: String,
    source_ref: Option<String>,
}

impl VerifiedSignature {
    /// Verified signature-bundle format.
    #[must_use]
    pub const fn scheme(&self) -> SignatureScheme {
        self.scheme
    }

    /// Digest of the exact verified bundle.
    #[must_use]
    pub const fn bundle_digest(&self) -> &ArtifactDigest {
        &self.bundle_digest
    }

    /// Repository identity checked by the verifier.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Workflow identity checked by the verifier.
    #[must_use]
    pub fn workflow(&self) -> &str {
        &self.workflow
    }

    /// Source tag checked by the verifier, when one was required.
    #[must_use]
    pub fn source_ref(&self) -> Option<&str> {
        self.source_ref.as_deref()
    }
}

/// A parsed release manifest whose exact source bytes have been authenticated.
pub struct AttestedReleaseManifest {
    manifest: ReleaseManifest,
    trust: ReleaseTrustPolicy,
    signature: VerifiedSignature,
    raw: Vec<u8>,
    bundle: Vec<u8>,
}

impl AttestedReleaseManifest {
    pub(crate) fn new(
        manifest: ReleaseManifest,
        trust: ReleaseTrustPolicy,
        signature: VerifiedSignature,
        raw: Vec<u8>,
        bundle: Vec<u8>,
    ) -> Self {
        Self {
            manifest,
            trust,
            signature,
            raw,
            bundle,
        }
    }

    /// Parsed authenticated manifest.
    #[must_use]
    pub const fn manifest(&self) -> &ReleaseManifest {
        &self.manifest
    }

    /// Signature proof for the manifest bytes.
    #[must_use]
    pub const fn signature(&self) -> &VerifiedSignature {
        &self.signature
    }

    /// Verify one platform artifact's checksum, exact bundle, and signer.
    ///
    /// Checksum and bundle-digest checks precede the external verifier. The
    /// returned value owns the exact verified bytes, preventing a caller from
    /// verifying one slice and publishing another.
    ///
    /// # Errors
    /// Missing platform rows, oversize artifacts/bundles, checksum or bundle
    /// substitution, and signature verification failures are refused.
    pub fn verify_artifact(
        &self,
        platform: &ReleasePlatform,
        bytes: Vec<u8>,
        bundle: &[u8],
        verifier: &dyn ReleaseSignatureVerifier,
    ) -> Result<VerifiedReleaseArtifact, UpdateError> {
        if bytes.len() > MAX_RELEASE_ARTIFACT_BYTES {
            return Err(UpdateError::ArtifactTooLarge {
                limit: MAX_RELEASE_ARTIFACT_BYTES,
            });
        }
        let artifact = self.manifest.artifact_for(platform)?;
        let staged = ArtifactDigest::of_bytes(&bytes);
        if &staged != artifact.digest() {
            return Err(mismatch(ArtifactMismatch::Digest {
                pinned: artifact.digest().clone(),
                staged,
            }));
        }
        verify_bundle_budget(bundle)?;
        let bundle_digest = ArtifactDigest::of_bytes(bundle);
        if &bundle_digest != artifact.attestation().bundle_digest() {
            return Err(UpdateError::SignatureBundleDigestMismatch {
                pinned: artifact.attestation().bundle_digest().clone(),
                found: bundle_digest,
            });
        }
        if artifact.attestation().scheme() != self.signature.scheme() {
            return Err(UpdateError::SignatureVerification {
                subject: SignatureSubject::Artifact,
                fault: SignatureVerificationFault::Unsupported,
            });
        }
        verifier
            .verify(SignatureVerificationRequest {
                subject: SignatureSubject::Artifact,
                subject_bytes: &bytes,
                bundle,
                trust: &self.trust,
            })
            .map_err(|fault| UpdateError::SignatureVerification {
                subject: SignatureSubject::Artifact,
                fault,
            })?;
        Ok(VerifiedReleaseArtifact {
            version: self.manifest.version().clone(),
            platform: platform.clone(),
            config_schema_version: self.manifest.config_schema_version(),
            plugin_api_version: self.manifest.plugin_api_version(),
            digest: artifact.digest().clone(),
            signature: VerifiedSignature {
                scheme: artifact.attestation().scheme(),
                bundle_digest: artifact.attestation().bundle_digest().clone(),
                repository: self.trust.repository().to_owned(),
                workflow: self.trust.workflow().to_owned(),
                source_ref: self.trust.source_ref().map(str::to_owned),
            },
            bytes,
            artifact_bundle: bundle.to_vec(),
            manifest_raw: self.raw.clone(),
            manifest_bundle: self.bundle.clone(),
            update_approval: None,
        })
    }
}

/// Exact signed and checksummed artifact bytes admitted for installation.
///
/// The type is intentionally not `Clone` or serializable. Only an authenticated
/// manifest can mint it, and [`crate::InstallRoot`] consumes it at publication.
pub struct VerifiedReleaseArtifact {
    pub(crate) version: ReleaseVersion,
    pub(crate) platform: ReleasePlatform,
    pub(crate) config_schema_version: u32,
    pub(crate) plugin_api_version: u32,
    pub(crate) digest: ArtifactDigest,
    pub(crate) signature: VerifiedSignature,
    pub(crate) bytes: Vec<u8>,
    pub(crate) artifact_bundle: Vec<u8>,
    pub(crate) manifest_raw: Vec<u8>,
    pub(crate) manifest_bundle: Vec<u8>,
    pub(crate) update_approval: Option<UpdateApproval>,
}

impl VerifiedReleaseArtifact {
    /// Release version bound by the authenticated manifest.
    #[must_use]
    pub const fn version(&self) -> &ReleaseVersion {
        &self.version
    }

    /// Exact target platform.
    #[must_use]
    pub const fn platform(&self) -> &ReleasePlatform {
        &self.platform
    }

    /// Configuration schema supported by this artifact.
    #[must_use]
    pub const fn config_schema_version(&self) -> u32 {
        self.config_schema_version
    }

    /// External plugin host API exposed by this artifact.
    #[must_use]
    pub const fn plugin_api_version(&self) -> u32 {
        self.plugin_api_version
    }

    /// Verified artifact digest.
    #[must_use]
    pub const fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }

    /// Verified signature identity and bundle.
    #[must_use]
    pub const fn signature(&self) -> &VerifiedSignature {
        &self.signature
    }

    /// Exact authenticated artifact bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn into_parts(self) -> VerifiedArtifactParts {
        VerifiedArtifactParts {
            version: self.version,
            platform: self.platform,
            config_schema_version: self.config_schema_version,
            plugin_api_version: self.plugin_api_version,
            digest: self.digest,
            bytes: self.bytes,
            artifact_bundle: self.artifact_bundle,
            manifest_raw: self.manifest_raw,
            manifest_bundle: self.manifest_bundle,
        }
    }
}

pub(crate) struct VerifiedArtifactParts {
    pub(crate) version: ReleaseVersion,
    pub(crate) platform: ReleasePlatform,
    pub(crate) config_schema_version: u32,
    pub(crate) plugin_api_version: u32,
    pub(crate) digest: ArtifactDigest,
    pub(crate) bytes: Vec<u8>,
    pub(crate) artifact_bundle: Vec<u8>,
    pub(crate) manifest_raw: Vec<u8>,
    pub(crate) manifest_bundle: Vec<u8>,
}

pub(crate) fn verify_manifest_signature(
    raw: &[u8],
    bundle: &[u8],
    trust: &ReleaseTrustPolicy,
    verifier: &dyn ReleaseSignatureVerifier,
) -> Result<VerifiedSignature, UpdateError> {
    verify_bundle_budget(bundle)?;
    verifier
        .verify(SignatureVerificationRequest {
            subject: SignatureSubject::Manifest,
            subject_bytes: raw,
            bundle,
            trust,
        })
        .map_err(|fault| UpdateError::SignatureVerification {
            subject: SignatureSubject::Manifest,
            fault,
        })?;
    Ok(VerifiedSignature {
        scheme: SignatureScheme::GithubSigstoreBundleV1,
        bundle_digest: ArtifactDigest::of_bytes(bundle),
        repository: trust.repository().to_owned(),
        workflow: trust.workflow().to_owned(),
        source_ref: trust.source_ref().map(str::to_owned),
    })
}

fn verify_bundle_budget(bundle: &[u8]) -> Result<(), UpdateError> {
    if bundle.is_empty() || bundle.len() > MAX_SIGNATURE_BUNDLE_BYTES {
        return Err(UpdateError::SignatureBundleSize {
            limit: MAX_SIGNATURE_BUNDLE_BYTES,
        });
    }
    Ok(())
}

fn valid_source_ref(value: &str) -> bool {
    let Some(tag) = value.strip_prefix("refs/tags/") else {
        return false;
    };
    !tag.is_empty()
        && value.len() <= MAX_SOURCE_REF_BYTES
        && !tag.starts_with('/')
        && !tag.ends_with('/')
        && !tag.contains("//")
        && !tag
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

fn valid_repository(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_REPOSITORY_BYTES || value.trim() != value {
        return false;
    }
    let mut parts = value.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(repository), None)
            if valid_github_component(owner) && valid_github_component(repository)
    )
}

fn valid_github_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_workflow(value: &str) -> bool {
    value.len() <= MAX_WORKFLOW_BYTES
        && value.starts_with(".github/workflows/")
        && (value.ends_with(".yml") || value.ends_with(".yaml"))
        && !value.contains("..")
        && !value.contains("//")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
}
