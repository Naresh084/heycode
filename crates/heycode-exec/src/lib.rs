//! Provider-neutral filesystem, subprocess, sandbox, and shell services.
//!
//! The filesystem contract owns explicit root authority, canonical capability
//! resolution, bounded reads/searches, observation-guarded atomic mutations,
//! metadata, and directories. The subprocess contract owns exact process
//! creation and lifecycle. Neither service chooses an approval decision; shell
//! defaulting and the shared sandbox workspace policy remain explicit layers.
//! The terminal registry owns persistent owner-scoped pseudo-terminal sessions
//! on top of that same process ownership model.

mod error;
mod exact;
mod filesystem;
mod local;
mod lsp;
mod model;
mod plugin;
mod retained_output;
mod sandbox;
mod service;
mod shell;
mod terminal;

pub use error::{ProcessError, ProcessErrorCode};
pub use exact::{ExactExecutable, ProcessAuthority};
pub use filesystem::{
    CheckedWriteOutput, CheckedWriteSpec, EditContext, EditFileOutput, EditFileSpec, FileEntryKind,
    FileMetadata, FileSystemBackend, FileSystemError, FileSystemErrorCode, FileSystemPolicy,
    FileSystemRoot, FileSystemRootAccess, FileSystemService, GlobOutput, GlobSpec, GrepContextLine,
    GrepFileMatch, GrepMatch, GrepOutput, GrepOutputMode, GrepSpec, MultiEditOutput, MultiEditSpec,
    ObservationLog, PathRequest, ReadFileOutput, ReadFilePage, ReadFileSpec, ReadFileWindow,
    ResolvedPath, SearchReport, WriteFileSpec, local_filesystem_plugin,
};
pub use lsp::{
    LspBackend, LspCallHierarchyEdge, LspDiagnostic, LspDiagnosticSeverity, LspDocumentRequest,
    LspError, LspErrorCode, LspHover, LspLocation, LspPosition, LspPositionRequest, LspRange,
    LspServerDefinition, LspServerDescriptor, LspServerId, LspService, LspSymbol,
    LspWorkspaceSymbolRequest, lsp_registry_plugin, lsp_stdio_plugin,
};
pub use model::{
    ContainmentMechanism, OutputOverflowPolicy, ProcessExit, ProcessId, ProcessOutput, ProcessSpec,
    SubprocessContainment,
};
pub use plugin::local_subprocess_plugin;
pub use retained_output::{
    DEFAULT_RETAINED_OUTPUT_MAX_OBJECT_BYTES, DEFAULT_RETAINED_OUTPUT_MAX_TOTAL_BYTES,
    MAX_RETAINED_OUTPUT_READ_BYTES, MAX_RETAINED_OUTPUT_VIEW_BYTES, RetainedOutputConfig,
    RetainedOutputError, RetainedOutputErrorCode, RetainedOutputId, RetainedOutputOwner,
    RetainedOutputRead, RetainedOutputReceipt, RetainedOutputService, retained_output_plugin,
};
pub use sandbox::{
    FileReadScope, FileWriteScope, NetworkScope, SERVICE_SANDBOX, Sandbox,
    SandboxBackendCapabilities, SandboxCapabilityReport, SandboxChoiceCapability, SandboxError,
    SandboxMode, SandboxPolicy, SandboxService, SandboxSupport, sandbox_service_plugin,
};
pub use service::{
    InteractiveProcess, MAX_PROCESS_OUTPUT_CHUNK_BYTES, OutputStream, ProcessInput,
    ProcessInputHandle, ProcessLineHandle, ProcessLines, ProcessOutputChunk, ProcessOutputHandle,
    ProcessOutputReader, ProcessOutputSink, RawInteractiveProcess,
};
pub use service::{ManagedProcess, ManagedProcessHandle, SubprocessBackend, SubprocessService};
pub use shell::{
    LocalShellConfig, ShellBackend, ShellId, ShellRequest, ShellService, ShellSpec,
    local_execution_plugin, local_execution_plugin_with_sandbox, local_shell_plugin,
    safe_environment_snapshot,
};
pub use terminal::{
    DEFAULT_TERMINAL_RETAINED_BYTES, MAX_TERMINAL_READ_BYTES, MAX_TERMINAL_RETAINED_BYTES,
    MAX_TERMINAL_SESSIONS, MAX_TERMINAL_SESSIONS_PER_OWNER, TerminalId, TerminalOwner,
    TerminalRead, TerminalService, TerminalSize, TerminalSpec, TerminalStatus,
    TerminalWorkspacePause, current_terminal_owner, terminal_registry_plugin, with_terminal_owner,
};

/// Plugin-owned subprocess service key.
pub const SERVICE_SUBPROCESS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("subprocess");

/// Plugin-owned resolved shell execution service key.
pub const SERVICE_SHELL: heycode_core::ServiceKey = heycode_core::ServiceKey::new("shell");

/// Plugin-owned replaceable filesystem service key.
pub const SERVICE_FILESYSTEM: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("filesystem");

/// Plugin-owned owner-scoped persistent terminal registry service key.
pub const SERVICE_TERMINAL: heycode_core::ServiceKey = heycode_core::ServiceKey::new("terminal");

/// Plugin-owned owner-scoped retained-output spill service key.
pub const SERVICE_RETAINED_OUTPUT: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("retained-output");

/// Plugin-owned language-server registry service key.
pub const SERVICE_LSP: heycode_core::ServiceKey = heycode_core::ServiceKey::new("lsp");
