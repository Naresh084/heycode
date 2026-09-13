//! GitHub artifact-attestation verifier over the composed process boundary.

use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use heycode_exec::{OutputOverflowPolicy, ProcessSpec, SubprocessService};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    ReleaseSignatureVerifier, SignatureSubject, SignatureVerificationFault,
    SignatureVerificationRequest,
};

const VERIFY_TIMEOUT: Duration = Duration::from_secs(90);
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const VERIFY_OUTPUT_LIMIT: usize = 64 * 1024;

/// Safe construction failure for the production GitHub verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GhAttestationVerifierError {
    /// The configured scratch root is not a safe absolute directory.
    #[error("release verification scratch root is invalid")]
    InvalidScratchRoot,
    /// The GitHub CLI executable could not be resolved through subprocess policy.
    #[error("GitHub attestation verifier is unavailable")]
    ProgramUnavailable,
}

/// GitHub CLI artifact verifier using an offline downloaded Sigstore bundle.
///
/// Verification still may consult the public Sigstore trusted-root service;
/// it receives no GitHub token or inherited process environment. The exact
/// subject/bundle bytes are written to one owner-only temporary directory and
/// all process output is discarded behind a closed result class.
pub struct GhAttestationVerifier {
    subprocess: SubprocessService,
    program: PathBuf,
    scratch_root: PathBuf,
    lifecycle: CancellationToken,
}

impl std::fmt::Debug for GhAttestationVerifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GhAttestationVerifier")
            .field("closed", &self.lifecycle.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl GhAttestationVerifier {
    /// Resolve the exact GitHub CLI and prepare an owner-controlled scratch root.
    ///
    /// # Errors
    /// Missing/non-executable `program`, relative/symlink/non-directory scratch
    /// roots, and filesystem failures are refused without exposing paths.
    pub fn new(
        subprocess: SubprocessService,
        program: impl AsRef<OsStr>,
        scratch_root: impl AsRef<Path>,
    ) -> Result<Self, GhAttestationVerifierError> {
        let program = subprocess
            .resolve_program(program.as_ref())
            .map_err(|_| GhAttestationVerifierError::ProgramUnavailable)?;
        let scratch_root = prepare_scratch_root(scratch_root.as_ref())?;
        let verifier = Self {
            subprocess,
            program,
            scratch_root,
            lifecycle: CancellationToken::new(),
        };
        verifier.probe()?;
        Ok(verifier)
    }

    /// Cancel an active verification and make held handles terminal. Idempotent.
    pub fn close(&self) {
        self.lifecycle.cancel();
    }

    fn verify_inner(
        &self,
        request: SignatureVerificationRequest<'_>,
    ) -> Result<(), SignatureVerificationFault> {
        if self.lifecycle.is_cancelled() {
            return Err(SignatureVerificationFault::Unavailable);
        }
        let temporary = tempfile::Builder::new()
            .prefix("verify-")
            .tempdir_in(&self.scratch_root)
            .map_err(|_| SignatureVerificationFault::Unavailable)?;
        let subject_path = temporary.path().join(match request.subject() {
            SignatureSubject::Manifest => "manifest.subject",
            SignatureSubject::Artifact => "artifact.subject",
        });
        let bundle_path = temporary.path().join("attestation.bundle.json");
        write_owner_file(&subject_path, request.subject_bytes())?;
        write_owner_file(&bundle_path, request.bundle())?;
        let signer_workflow = format!(
            "github.com/{}/{}",
            request.expected_repository(),
            request.expected_workflow()
        );
        let source_ref = request
            .expected_source_ref()
            .ok_or(SignatureVerificationFault::Unsupported)?;
        let arguments = vec![
            OsString::from("attestation"),
            OsString::from("verify"),
            subject_path.into_os_string(),
            OsString::from("--repo"),
            OsString::from(request.expected_repository()),
            OsString::from("--bundle"),
            bundle_path.into_os_string(),
            OsString::from("--signer-workflow"),
            OsString::from(signer_workflow),
            OsString::from("--cert-oidc-issuer"),
            OsString::from(request.expected_issuer()),
            OsString::from("--source-ref"),
            OsString::from(source_ref),
            OsString::from("--deny-self-hosted-runners"),
        ];
        let environment = isolated_environment(temporary.path());
        let spec = ProcessSpec::new(&self.program, temporary.path())
            .and_then(|spec| spec.with_args(arguments))
            .and_then(|spec| spec.with_environment(environment))
            .and_then(|spec| spec.with_timeout(Some(VERIFY_TIMEOUT)))
            .and_then(|spec| spec.with_output_limit_bytes(VERIFY_OUTPUT_LIMIT))
            .map(|spec| spec.with_output_overflow_policy(OutputOverflowPolicy::Error))
            .map_err(|_| SignatureVerificationFault::Unavailable)?;
        run_verifier(self.subprocess.clone(), spec, self.lifecycle.child_token())
    }

    fn probe(&self) -> Result<(), GhAttestationVerifierError> {
        let spec = ProcessSpec::new(&self.program, &self.scratch_root)
            .and_then(|spec| spec.with_args(["attestation", "verify", "--help"]))
            .and_then(|spec| spec.with_environment(isolated_environment(&self.scratch_root)))
            .and_then(|spec| spec.with_timeout(Some(PROBE_TIMEOUT)))
            .and_then(|spec| spec.with_output_limit_bytes(VERIFY_OUTPUT_LIMIT))
            .map(|spec| spec.with_output_overflow_policy(OutputOverflowPolicy::Error))
            .map_err(|_| GhAttestationVerifierError::ProgramUnavailable)?;
        run_verifier(self.subprocess.clone(), spec, self.lifecycle.child_token())
            .map_err(|_| GhAttestationVerifierError::ProgramUnavailable)
    }
}

impl ReleaseSignatureVerifier for GhAttestationVerifier {
    fn verify(
        &self,
        request: SignatureVerificationRequest<'_>,
    ) -> Result<(), SignatureVerificationFault> {
        self.verify_inner(request)
    }
}

fn run_verifier(
    subprocess: SubprocessService,
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> Result<(), SignatureVerificationFault> {
    let worker = std::thread::Builder::new()
        .name("heycode-release-verifier".to_owned())
        .spawn(move || run_verifier_worker(subprocess, spec, cancellation))
        .map_err(|_| SignatureVerificationFault::Unavailable)?;
    worker
        .join()
        .map_err(|_| SignatureVerificationFault::Unavailable)?
}

fn run_verifier_worker(
    subprocess: SubprocessService,
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> Result<(), SignatureVerificationFault> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| SignatureVerificationFault::Unavailable)?;
    runtime.block_on(async move {
        let output = subprocess
            .output(spec, cancellation)
            .await
            .map_err(|_| SignatureVerificationFault::Unavailable)?;
        if output.exit().is_success() {
            Ok(())
        } else {
            Err(SignatureVerificationFault::Invalid)
        }
    })
}

fn prepare_scratch_root(path: &Path) -> Result<PathBuf, GhAttestationVerifierError> {
    if !path.is_absolute() {
        return Err(GhAttestationVerifierError::InvalidScratchRoot);
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(GhAttestationVerifierError::InvalidScratchRoot);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)
                .map_err(|_| GhAttestationVerifierError::InvalidScratchRoot)?;
        }
        Err(_) => return Err(GhAttestationVerifierError::InvalidScratchRoot),
    }
    set_owner_directory_permissions(path)?;
    std::fs::canonicalize(path).map_err(|_| GhAttestationVerifierError::InvalidScratchRoot)
}

fn isolated_environment(root: &Path) -> Vec<(OsString, OsString)> {
    let root = root.as_os_str().to_owned();
    vec![
        (OsString::from("GH_CONFIG_DIR"), root.clone()),
        (OsString::from("GH_PROMPT_DISABLED"), OsString::from("1")),
        (OsString::from("HOME"), root.clone()),
        (OsString::from("NO_COLOR"), OsString::from("1")),
        (OsString::from("USERPROFILE"), root.clone()),
        (OsString::from("XDG_CACHE_HOME"), root),
    ]
}

fn write_owner_file(path: &Path, bytes: &[u8]) -> Result<(), SignatureVerificationFault> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| SignatureVerificationFault::Unavailable)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| SignatureVerificationFault::Unavailable)
}

fn set_owner_directory_permissions(path: &Path) -> Result<(), GhAttestationVerifierError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| GhAttestationVerifierError::InvalidScratchRoot)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
