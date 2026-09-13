//! Mandatory effective sandbox policy service for local process launches.

use std::path::PathBuf;
use std::sync::Arc;

use crate::model::validate_absolute_path;
use crate::{ProcessAuthority, ProcessError, ProcessErrorCode, ProcessSpec};

/// Process sandbox service key.
pub const SERVICE_SANDBOX: heycode_core::ServiceKey = heycode_core::ServiceKey::new("sandbox");

/// Effective confinement strength for every local process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Explicitly run without OS confinement while retaining the policy path.
    Off,
    /// Read access with writes denied by the backend.
    ReadOnly,
    /// Writes permitted only under the workspace root and backend temp roots.
    WorkspaceWrite,
}

impl SandboxMode {
    /// Stable machine identifier used by settings/UI/report projections.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "full_access",
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
        }
    }
}

/// Evidence that one backend guarantee is enforceable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxSupport {
    /// Backend implementation and preflight support the guarantee.
    Supported,
    /// Backend explicitly cannot enforce the guarantee.
    Unsupported,
    /// Evidence is insufficient to offer the guarantee.
    Unknown,
}

/// Static enforcement facts published by one backend implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxBackendCapabilities {
    /// Deny host filesystem writes while retaining host reads.
    pub read_only: SandboxSupport,
    /// Restrict writes to workspace and isolated/provider temp roots.
    pub workspace_write: SandboxSupport,
    /// Isolate or deny host networking.
    pub network_isolation: SandboxSupport,
}

/// Files visible for reading under one choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReadScope {
    /// Host filesystem remains broadly readable subject to ordinary OS
    /// permissions; backend-specific virtual mounts may replace some paths.
    Host,
    /// No enforceable scope is available.
    Unspecified,
}

/// Files writable under one choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileWriteScope {
    /// Ordinary host permissions apply without an added boundary.
    Host,
    /// Only device sinks required for process operation are writable.
    DeviceOnly,
    /// Workspace and backend-defined temporary roots are writable.
    /// This does not imply those temporary roots are isolated from the host.
    WorkspaceAndTemp,
    /// No enforceable scope is available.
    Unspecified,
}

/// Network visibility under one choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkScope {
    /// Host networking remains available subject to ordinary OS controls.
    Host,
    /// Network access is isolated or denied by the backend.
    Isolated,
    /// No enforceable scope is available.
    Unspecified,
}

/// One user-selectable policy choice and its exact guarantees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxChoiceCapability {
    /// Choice id/effective mode.
    pub mode: SandboxMode,
    /// Whether this host/backend can enforce the choice now.
    pub selectable: bool,
    /// Effective read visibility if selected.
    pub file_read: FileReadScope,
    /// Effective write visibility if selected.
    pub file_write: FileWriteScope,
    /// Effective network visibility if selected.
    pub network: NetworkScope,
}

/// Safe effective/available sandbox report consumed by UI and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCapabilityReport {
    /// Currently effective choice.
    pub effective_mode: SandboxMode,
    /// Backend actively wrapping launches, absent for full access.
    pub active_backend: Option<&'static str>,
    /// Backend available for restrictive choices on this host.
    pub available_backend: Option<&'static str>,
    /// Full access, read-only and workspace-write rows in stable order.
    pub choices: Vec<SandboxChoiceCapability>,
}

impl SandboxCapabilityReport {
    /// Find the row for one mode.
    #[must_use]
    pub fn choice(&self, mode: SandboxMode) -> Option<&SandboxChoiceCapability> {
        self.choices.iter().find(|choice| choice.mode == mode)
    }
}

/// Complete effective per-launch policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// Effective mode.
    pub mode: SandboxMode,
    /// Absolute workspace boundary.
    pub workspace_root: PathBuf,
}

/// Backend failure at the explicit policy boundary.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SandboxError {
    message: String,
}

impl SandboxError {
    /// Build a provider failure. Backends should keep this text bounded and
    /// secret-free; subprocess callers receive only a fixed error class.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// OS-specific argv confinement Provider.
pub trait Sandbox: Send + Sync {
    /// Stable backend id.
    fn name(&self) -> &'static str;

    /// Static + preflighted enforcement facts for selection/reporting.
    fn capabilities(&self) -> SandboxBackendCapabilities;

    /// Transform exact argv for execution under `policy`.
    ///
    /// # Errors
    /// Unsupported/unavailable/malformed confinement fails closed.
    fn confine(&self, argv: &[String], policy: &SandboxPolicy)
    -> Result<Vec<String>, SandboxError>;
}

/// Effective sandbox mode plus optional OS backend.
#[derive(Clone)]
pub struct SandboxService {
    policy: SandboxPolicy,
    backend: Option<Arc<dyn Sandbox>>,
    workspace_identity: Option<crate::filesystem::Stamp>,
}

impl SandboxService {
    /// Validate one effective service generation.
    ///
    /// # Errors
    /// Workspace roots must be absolute, existing, canonicalizable directories
    /// whose identity can be pinned. Active modes require a backend that
    /// explicitly supports the selected guarantee.
    pub fn new(
        mode: SandboxMode,
        workspace_root: impl Into<PathBuf>,
        backend: Option<Arc<dyn Sandbox>>,
    ) -> Result<Self, SandboxError> {
        let workspace_root = workspace_root.into();
        validate_absolute_path(&workspace_root)
            .map_err(|_| SandboxError::new("sandbox workspace root must be absolute"))?;
        let workspace_root = std::fs::canonicalize(&workspace_root).map_err(|_| {
            SandboxError::new("sandbox workspace root must exist and be accessible")
        })?;
        if !workspace_root.is_dir() {
            return Err(SandboxError::new(
                "sandbox workspace root must be a directory",
            ));
        }
        let workspace_directory =
            cap_std::fs::Dir::open_ambient_dir(&workspace_root, cap_std::ambient_authority())
                .map_err(|_| SandboxError::new("sandbox workspace identity could not be pinned"))?;
        if std::fs::canonicalize(&workspace_root).ok().as_deref() != Some(workspace_root.as_path())
        {
            return Err(SandboxError::new(
                "sandbox workspace root changed during activation",
            ));
        }
        let workspace_identity = crate::filesystem::Stamp::of_dir(&workspace_directory)
            .ok_or_else(|| SandboxError::new("sandbox workspace identity could not be pinned"))?;
        if !matches!(mode, SandboxMode::Off) && backend.is_none() {
            return Err(SandboxError::new(
                "active sandbox mode requires one available backend",
            ));
        }
        if let Some(backend) = &backend {
            let support = match mode {
                SandboxMode::Off => SandboxSupport::Supported,
                SandboxMode::ReadOnly => backend.capabilities().read_only,
                SandboxMode::WorkspaceWrite => backend.capabilities().workspace_write,
            };
            if support != SandboxSupport::Supported {
                return Err(SandboxError::new(
                    "selected sandbox mode is not enforceable by the backend",
                ));
            }
        }
        Ok(Self {
            policy: SandboxPolicy {
                mode,
                workspace_root,
            },
            backend,
            workspace_identity: Some(workspace_identity),
        })
    }

    pub(crate) fn standalone_off() -> Self {
        let workspace_root = std::env::current_dir()
            .ok()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .or_else(|| std::fs::canonicalize(std::env::temp_dir()).ok())
            .unwrap_or_else(std::env::temp_dir);
        Self {
            policy: SandboxPolicy {
                mode: SandboxMode::Off,
                workspace_root: workspace_root.clone(),
            },
            backend: None,
            workspace_identity: cap_std::fs::Dir::open_ambient_dir(
                &workspace_root,
                cap_std::ambient_authority(),
            )
            .ok()
            .and_then(|directory| crate::filesystem::Stamp::of_dir(&directory)),
        }
    }

    /// Apply the same enforced mode/backend to a host-created child workspace.
    /// This re-pins the directory identity; it never turns a restrictive mode off.
    /// The host must own/authorize the new workspace (e.g. a managed Git lease).
    pub fn for_workspace(&self, workspace_root: impl Into<PathBuf>) -> Result<Self, SandboxError> {
        Self::new(self.policy.mode, workspace_root, self.backend.clone())
    }

    /// Effective policy snapshot.
    #[must_use]
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Available backend id, including a candidate while effective mode is off.
    #[must_use]
    pub fn backend_name(&self) -> Option<&'static str> {
        self.backend.as_ref().map(|backend| backend.name())
    }

    /// Effective mode, backend availability and exact choice guarantees.
    #[must_use]
    pub fn capability_report(&self) -> SandboxCapabilityReport {
        let backend = self.backend.as_ref();
        let capabilities = backend.map(|backend| backend.capabilities());
        let choice = |mode, support, file_write| {
            let selectable = support == SandboxSupport::Supported;
            SandboxChoiceCapability {
                mode,
                selectable,
                file_read: if selectable {
                    FileReadScope::Host
                } else {
                    FileReadScope::Unspecified
                },
                file_write: if selectable {
                    file_write
                } else {
                    FileWriteScope::Unspecified
                },
                network: if selectable {
                    match capabilities
                        .as_ref()
                        .map_or(SandboxSupport::Unsupported, |facts| facts.network_isolation)
                    {
                        SandboxSupport::Supported => NetworkScope::Isolated,
                        SandboxSupport::Unsupported | SandboxSupport::Unknown => NetworkScope::Host,
                    }
                } else {
                    NetworkScope::Unspecified
                },
            }
        };
        let read_only = capabilities
            .as_ref()
            .map_or(SandboxSupport::Unsupported, |facts| facts.read_only);
        let workspace_write = capabilities
            .as_ref()
            .map_or(SandboxSupport::Unsupported, |facts| facts.workspace_write);
        SandboxCapabilityReport {
            effective_mode: self.policy.mode,
            active_backend: if matches!(self.policy.mode, SandboxMode::Off) {
                None
            } else {
                self.backend_name()
            },
            available_backend: self.backend_name(),
            choices: vec![
                SandboxChoiceCapability {
                    mode: SandboxMode::Off,
                    selectable: true,
                    file_read: FileReadScope::Host,
                    file_write: FileWriteScope::Host,
                    network: NetworkScope::Host,
                },
                choice(SandboxMode::ReadOnly, read_only, FileWriteScope::DeviceOnly),
                choice(
                    SandboxMode::WorkspaceWrite,
                    workspace_write,
                    FileWriteScope::WorkspaceAndTemp,
                ),
            ],
        }
    }

    pub(crate) fn confine(&self, spec: ProcessSpec) -> Result<ProcessSpec, ProcessError> {
        if matches!(self.policy.mode, SandboxMode::Off) {
            return Ok(spec);
        }
        let root_is_stable = self.workspace_identity.is_some_and(|expected| {
            cap_std::fs::Dir::open_ambient_dir(
                &self.policy.workspace_root,
                cap_std::ambient_authority(),
            )
            .ok()
            .and_then(|directory| crate::filesystem::Stamp::of_dir(&directory))
            .is_some_and(|current| expected.same_identity(current))
        });
        if !root_is_stable {
            return Err(ProcessError::new(ProcessErrorCode::Sandbox));
        }
        let Some(backend) = &self.backend else {
            return Ok(spec);
        };
        let argv = spec
            .launch_argv_strings()
            .map_err(|_| ProcessError::new(ProcessErrorCode::Sandbox))?;
        let wrapped = backend
            .confine(&argv, &self.policy)
            .map_err(|_| ProcessError::new(ProcessErrorCode::Sandbox))?;
        spec.with_launch_argv(wrapped)
            .map_err(|_| ProcessError::new(ProcessErrorCode::Sandbox))
    }

    pub(crate) fn confine_exact(
        &self,
        spec: ProcessSpec,
        authority: ProcessAuthority,
    ) -> Result<ProcessSpec, ProcessError> {
        let report = self.capability_report();
        let Some(choice) = report.choice(self.policy.mode) else {
            return Err(ProcessError::new(ProcessErrorCode::Sandbox));
        };
        if !choice.selectable
            || (matches!(choice.file_read, FileReadScope::Host)
                && !authority.filesystem_read())
            || (matches!(
                choice.file_write,
                FileWriteScope::Host | FileWriteScope::WorkspaceAndTemp
            ) && !authority.filesystem_write())
            || (matches!(choice.network, NetworkScope::Host) && !authority.network())
            // Existing native backends do not claim descendant-creation
            // denial. A child that was not granted process spawning cannot be
            // launched until a backend publishes and enforces that fact.
            || !authority.process_spawn()
        {
            return Err(ProcessError::new(ProcessErrorCode::Sandbox));
        }
        self.confine(spec)
    }
}

impl std::fmt::Debug for SandboxService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxService")
            .field("mode", &self.policy.mode)
            .field("workspace_root", &"<redacted>")
            .field("backend", &self.backend_name())
            .finish()
    }
}

/// Publish an already-resolved sandbox service for embedded/test worlds.
#[must_use]
pub fn sandbox_service_plugin(service: SandboxService) -> Box<dyn heycode_core::Plugin> {
    struct SandboxPolicyPlugin(SandboxService);

    impl heycode_core::Plugin for SandboxPolicyPlugin {
        fn name(&self) -> &'static str {
            "sandbox-policy"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SANDBOX]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            context.provide(SERVICE_SANDBOX, self.name(), self.0.clone())
        }
    }

    Box::new(SandboxPolicyPlugin(service))
}
