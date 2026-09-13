//! Replaceable LSP service and local stdio Provider.
//!
//! The local Provider owns language-server definitions and lazy live sessions.
//! Every definition carries an exact interactive [`ProcessSpec`] rooted at one
//! provider-resolved workspace. Servers launch only through the composed
//! [`SubprocessService`], so the common sandbox, explicit environment and
//! whole-tree cancellation path apply unchanged.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor,
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    FileSystemService, ManagedProcess, PathRequest, ProcessError, ProcessErrorCode, ProcessInput,
    ProcessOutputChunk, ProcessOutputReader, ProcessSpec, ReadFileSpec, ResolvedPath,
    SERVICE_FILESYSTEM, SERVICE_LSP, SERVICE_SUBPROCESS, SubprocessService,
};

const MAX_SERVER_ID_BYTES: usize = 64;
const MAX_LANGUAGE_ID_BYTES: usize = 64;
const MAX_LSP_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_LSP_HEADER_BYTES: usize = 8 * 1024;
const MAX_LSP_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_LSP_LOCATIONS: usize = 4096;
const MAX_LSP_DIAGNOSTICS: usize = 4096;
const MAX_LSP_SYMBOLS: usize = 4096;
const MAX_LSP_CALLS: usize = 4096;
const MAX_LSP_PREPARED_CALL_ITEMS: usize = 32;
const MAX_LSP_TEXT_BYTES: usize = 4096;
const MAX_LSP_HOVER_BYTES: usize = 64 * 1024;
const PUSH_DIAGNOSTICS_WAIT_MS: u64 = 2_000;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Stable LSP failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LspErrorCode {
    /// Caller/config/provider boundary input is invalid.
    InvalidSpec,
    /// A server id is already registered.
    DuplicateServer,
    /// No server with this id is registered.
    UnknownServer,
    /// Caller cancelled an operation.
    Cancelled,
    /// Registry or selected server generation stopped.
    ServiceStopped,
    /// Server launch failed through the subprocess service.
    Spawn,
    /// LSP framing, JSON-RPC or result shape is invalid.
    Protocol,
    /// A frame/document/result exceeded its explicit bound.
    OutputLimit,
    /// Operation is not supported by this Provider/server.
    Unsupported,
    /// Filesystem/process I/O failed safely.
    Io,
}

impl LspErrorCode {
    /// Stable machine name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid_spec",
            Self::DuplicateServer => "duplicate_server",
            Self::UnknownServer => "unknown_server",
            Self::Cancelled => "cancelled",
            Self::ServiceStopped => "service_stopped",
            Self::Spawn => "spawn",
            Self::Protocol => "protocol",
            Self::OutputLimit => "output_limit",
            Self::Unsupported => "unsupported",
            Self::Io => "io",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid LSP specification",
            Self::DuplicateServer => "LSP server id is already registered",
            Self::UnknownServer => "LSP server is not registered",
            Self::Cancelled => "LSP operation was cancelled",
            Self::ServiceStopped => "LSP service has stopped",
            Self::Spawn => "LSP server could not be started",
            Self::Protocol => "LSP server protocol failed",
            Self::OutputLimit => "LSP payload exceeded its configured limit",
            Self::Unsupported => "LSP operation is unsupported",
            Self::Io => "LSP operation failed",
        }
    }
}

/// Fixed request/body/path/argv-free LSP failure.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct LspError {
    code: LspErrorCode,
    message: &'static str,
}

impl LspError {
    const fn new(code: LspErrorCode) -> Self {
        Self {
            code,
            message: code.message(),
        }
    }

    /// Construct one fixed body/path/argv-free provider failure.
    #[must_use]
    pub const fn from_code(code: LspErrorCode) -> Self {
        Self::new(code)
    }

    /// Stable failure class.
    #[must_use]
    pub const fn code(&self) -> LspErrorCode {
        self.code
    }
}

/// Validated language-server identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LspServerId(String);

impl LspServerId {
    /// Validate a lowercase kebab-case id.
    ///
    /// # Errors
    /// Empty, oversized or malformed ids fail.
    pub fn new(value: impl Into<String>) -> Result<Self, LspError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (1..=MAX_SERVER_ID_BYTES).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !value.contains("--");
        if valid {
            Ok(Self(value))
        } else {
            Err(LspError::new(LspErrorCode::InvalidSpec))
        }
    }

    /// Stable id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Zero-based LSP line/UTF-16-code-unit position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LspPosition {
    line: u32,
    character: u32,
}

impl LspPosition {
    /// Construct a zero-based LSP position.
    #[must_use]
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }

    /// Zero-based line.
    #[must_use]
    pub const fn line(self) -> u32 {
        self.line
    }

    /// Zero-based UTF-16 code-unit offset.
    #[must_use]
    pub const fn character(self) -> u32 {
        self.character
    }
}

/// Validated half-open LSP range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LspRange {
    start: LspPosition,
    end: LspPosition,
}

impl LspRange {
    /// Build a range whose end does not precede its start.
    ///
    /// # Errors
    /// Reversed ranges fail instead of being reordered.
    pub const fn new(start: LspPosition, end: LspPosition) -> Result<Self, LspError> {
        if end.line < start.line || (end.line == start.line && end.character < start.character) {
            Err(LspError::new(LspErrorCode::InvalidSpec))
        } else {
            Ok(Self { start, end })
        }
    }

    /// Start position.
    #[must_use]
    pub const fn start(self) -> LspPosition {
        self.start
    }

    /// End position.
    #[must_use]
    pub const fn end(self) -> LspPosition {
        self.end
    }
}

/// Exact local stdio server definition.
#[derive(Clone)]
pub struct LspServerDefinition {
    id: LspServerId,
    language_id: String,
    workspace: ResolvedPath,
    process: ProcessSpec,
}

impl LspServerDefinition {
    /// Bind one exact interactive process to one resolved workspace/language.
    ///
    /// # Errors
    /// Invalid language ids, noninteractive processes or a process cwd that
    /// differs from the resolved workspace fail.
    pub fn new(
        id: LspServerId,
        language_id: impl Into<String>,
        workspace: ResolvedPath,
        process: ProcessSpec,
    ) -> Result<Self, LspError> {
        let language_id = language_id.into();
        let valid_language = (1..=MAX_LANGUAGE_ID_BYTES).contains(&language_id.len())
            && language_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+')
            });
        if !valid_language || !process.interactive_stdio() || process.cwd() != workspace.as_path() {
            return Err(LspError::new(LspErrorCode::InvalidSpec));
        }
        Ok(Self {
            id,
            language_id,
            workspace,
            process,
        })
    }

    /// Server id.
    #[must_use]
    pub const fn id(&self) -> &LspServerId {
        &self.id
    }

    /// LSP language id used for document synchronization.
    #[must_use]
    pub fn language_id(&self) -> &str {
        &self.language_id
    }

    /// Provider-resolved workspace root.
    #[must_use]
    pub const fn workspace(&self) -> &ResolvedPath {
        &self.workspace
    }

    /// Exact argv/cwd/environment process spec.
    #[must_use]
    pub const fn process(&self) -> &ProcessSpec {
        &self.process
    }
}

impl std::fmt::Debug for LspServerDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspServerDefinition")
            .field("id", &self.id)
            .field("language_id", &self.language_id)
            .field("workspace", &"[REDACTED]")
            .field("process", &self.process)
            .finish()
    }
}

/// Safe registered language-server identity exposed to model-facing Consumers.
///
/// Workspace paths and process details are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerDescriptor {
    id: LspServerId,
    language_id: String,
}

impl LspServerDescriptor {
    /// Construct one provider descriptor from validated identity parts.
    ///
    /// # Errors
    /// Empty, oversized or malformed language ids fail.
    pub fn new(id: LspServerId, language_id: impl Into<String>) -> Result<Self, LspError> {
        let language_id = language_id.into();
        let valid_language = (1..=MAX_LANGUAGE_ID_BYTES).contains(&language_id.len())
            && language_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+')
            });
        if !valid_language {
            return Err(LspError::new(LspErrorCode::InvalidSpec));
        }
        Ok(Self { id, language_id })
    }

    /// Registered server id.
    #[must_use]
    pub const fn id(&self) -> &LspServerId {
        &self.id
    }

    /// LSP language id.
    #[must_use]
    pub fn language_id(&self) -> &str {
        &self.language_id
    }
}

/// Definition/reference request for one resolved document position.
#[derive(Clone)]
pub struct LspPositionRequest {
    server: LspServerId,
    path: ResolvedPath,
    position: LspPosition,
}

impl LspPositionRequest {
    /// Construct from already validated boundary types.
    #[must_use]
    pub const fn new(server: LspServerId, path: ResolvedPath, position: LspPosition) -> Self {
        Self {
            server,
            path,
            position,
        }
    }
}

impl std::fmt::Debug for LspPositionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspPositionRequest")
            .field("server", &self.server)
            .field("path", &"[REDACTED]")
            .field("position", &self.position)
            .finish()
    }
}

/// Diagnostics request for one resolved document.
#[derive(Clone)]
pub struct LspDocumentRequest {
    server: LspServerId,
    path: ResolvedPath,
}

impl LspDocumentRequest {
    /// Construct from already validated boundary types.
    #[must_use]
    pub const fn new(server: LspServerId, path: ResolvedPath) -> Self {
        Self { server, path }
    }
}

impl std::fmt::Debug for LspDocumentRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspDocumentRequest")
            .field("server", &self.server)
            .field("path", &"[REDACTED]")
            .finish()
    }
}

/// Normalized definition/reference location.
#[derive(Clone, PartialEq, Eq)]
pub struct LspLocation {
    path: ResolvedPath,
    range: LspRange,
}

impl LspLocation {
    /// Construct one already-resolved provider result.
    #[must_use]
    pub const fn new(path: ResolvedPath, range: LspRange) -> Self {
        Self { path, range }
    }

    /// Provider-resolved target path.
    #[must_use]
    pub const fn path(&self) -> &ResolvedPath {
        &self.path
    }

    /// Target range.
    #[must_use]
    pub const fn range(&self) -> LspRange {
        self.range
    }
}

impl std::fmt::Debug for LspLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspLocation")
            .field("path", &"[REDACTED]")
            .field("range", &self.range)
            .finish()
    }
}

/// Normalized diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LspDiagnosticSeverity {
    /// LSP severity 1.
    Error,
    /// LSP severity 2.
    Warning,
    /// LSP severity 3.
    Information,
    /// LSP severity 4.
    Hint,
    /// Missing or future severity value.
    Unknown,
}

/// One bounded normalized document diagnostic.
#[derive(Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    path: ResolvedPath,
    range: LspRange,
    severity: LspDiagnosticSeverity,
    message: String,
    source: Option<String>,
}

impl LspDiagnostic {
    /// Construct one bounded provider diagnostic.
    ///
    /// # Errors
    /// Empty, oversized or control-bearing message/source values fail.
    pub fn new(
        path: ResolvedPath,
        range: LspRange,
        severity: LspDiagnosticSeverity,
        message: impl Into<String>,
        source: Option<String>,
    ) -> Result<Self, LspError> {
        let message = message.into();
        if !valid_lsp_text(&message)
            || source
                .as_deref()
                .is_some_and(|source| !valid_lsp_text(source))
        {
            return Err(LspError::new(LspErrorCode::InvalidSpec));
        }
        Ok(Self {
            path,
            range,
            severity,
            message,
            source,
        })
    }

    /// Document carrying this diagnostic.
    #[must_use]
    pub const fn path(&self) -> &ResolvedPath {
        &self.path
    }

    /// Diagnostic range.
    #[must_use]
    pub const fn range(&self) -> LspRange {
        self.range
    }

    /// Severity evidence.
    #[must_use]
    pub const fn severity(&self) -> LspDiagnosticSeverity {
        self.severity
    }

    /// Bounded server message. This accessor is the explicit presentation
    /// boundary; Debug never includes it.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Optional bounded server/source label.
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
}

impl std::fmt::Debug for LspDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspDiagnostic")
            .field("path", &"[REDACTED]")
            .field("range", &self.range)
            .field("severity", &self.severity)
            .field("message_bytes", &self.message.len())
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

/// Bounded hover/type information returned by a language server.
#[derive(Clone, PartialEq, Eq)]
pub struct LspHover {
    contents: String,
    range: Option<LspRange>,
}

impl LspHover {
    /// Construct bounded hover text and its optional source range.
    ///
    /// # Errors
    /// Empty, oversized or unsafe control-bearing text fails.
    pub fn new(contents: impl Into<String>, range: Option<LspRange>) -> Result<Self, LspError> {
        let contents = contents.into();
        if contents.is_empty()
            || contents.len() > MAX_LSP_HOVER_BYTES
            || contains_unsafe_control(&contents)
        {
            return Err(LspError::new(LspErrorCode::InvalidSpec));
        }
        Ok(Self { contents, range })
    }

    /// Server-authored Markdown/plain-text type information.
    #[must_use]
    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// Optional range to which the hover applies.
    #[must_use]
    pub const fn range(&self) -> Option<LspRange> {
        self.range
    }
}

impl std::fmt::Debug for LspHover {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspHover")
            .field("contents_bytes", &self.contents.len())
            .field("range", &self.range)
            .finish()
    }
}

/// One normalized document/workspace symbol.
#[derive(Clone, PartialEq, Eq)]
pub struct LspSymbol {
    name: String,
    detail: Option<String>,
    kind: u32,
    path: ResolvedPath,
    range: LspRange,
    selection_range: LspRange,
    container_name: Option<String>,
}

impl LspSymbol {
    /// Construct one bounded symbol row.
    ///
    /// # Errors
    /// Empty, oversized or control-bearing display text fails.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        detail: Option<String>,
        kind: u32,
        path: ResolvedPath,
        range: LspRange,
        selection_range: LspRange,
        container_name: Option<String>,
    ) -> Result<Self, LspError> {
        let name = name.into();
        if !valid_lsp_text(&name)
            || detail
                .as_deref()
                .is_some_and(|value| !valid_lsp_text(value))
            || container_name
                .as_deref()
                .is_some_and(|value| !valid_lsp_text(value))
        {
            return Err(LspError::new(LspErrorCode::InvalidSpec));
        }
        Ok(Self {
            name,
            detail,
            kind,
            path,
            range,
            selection_range,
            container_name,
        })
    }

    /// Symbol name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Optional detail/type label.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Numeric LSP `SymbolKind`, retained without guessing future values.
    #[must_use]
    pub const fn kind(&self) -> u32 {
        self.kind
    }

    /// Provider-resolved file path.
    #[must_use]
    pub const fn path(&self) -> &ResolvedPath {
        &self.path
    }

    /// Full symbol range.
    #[must_use]
    pub const fn range(&self) -> LspRange {
        self.range
    }

    /// Name/selection range.
    #[must_use]
    pub const fn selection_range(&self) -> LspRange {
        self.selection_range
    }

    /// Optional containing symbol name.
    #[must_use]
    pub fn container_name(&self) -> Option<&str> {
        self.container_name.as_deref()
    }
}

impl std::fmt::Debug for LspSymbol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspSymbol")
            .field("name_bytes", &self.name.len())
            .field("kind", &self.kind)
            .field("path", &"[REDACTED]")
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

/// Workspace-symbol query against one configured server.
#[derive(Clone)]
pub struct LspWorkspaceSymbolRequest {
    server: LspServerId,
    query: String,
}

impl LspWorkspaceSymbolRequest {
    /// Construct a bounded query. Empty queries are valid LSP and mean list.
    ///
    /// # Errors
    /// Oversized or unsafe control-bearing text fails.
    pub fn new(server: LspServerId, query: impl Into<String>) -> Result<Self, LspError> {
        let query = query.into();
        if query.len() > MAX_LSP_TEXT_BYTES || contains_unsafe_control(&query) {
            return Err(LspError::new(LspErrorCode::InvalidSpec));
        }
        Ok(Self { server, query })
    }
}

impl std::fmt::Debug for LspWorkspaceSymbolRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspWorkspaceSymbolRequest")
            .field("server", &self.server)
            .field("query_bytes", &self.query.len())
            .finish()
    }
}

/// One normalized incoming/outgoing call-hierarchy edge.
#[derive(Clone, PartialEq, Eq)]
pub struct LspCallHierarchyEdge {
    symbol: LspSymbol,
    call_ranges: Vec<LspRange>,
}

impl LspCallHierarchyEdge {
    /// Construct an edge from a normalized peer symbol and bounded call sites.
    #[must_use]
    pub fn new(symbol: LspSymbol, call_ranges: Vec<LspRange>) -> Self {
        Self {
            symbol,
            call_ranges,
        }
    }

    /// Peer symbol (`from` for incoming, `to` for outgoing).
    #[must_use]
    pub const fn symbol(&self) -> &LspSymbol {
        &self.symbol
    }

    /// Call-site ranges in the prepared item.
    #[must_use]
    pub fn call_ranges(&self) -> &[LspRange] {
        &self.call_ranges
    }
}

impl std::fmt::Debug for LspCallHierarchyEdge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspCallHierarchyEdge")
            .field("symbol", &self.symbol)
            .field("call_ranges", &self.call_ranges)
            .finish()
    }
}

/// Replaceable language-server Provider behind [`LspService`].
#[async_trait]
pub trait LspBackend: Send + Sync {
    /// Register one stdio definition as a context effect.
    ///
    /// # Errors
    /// Duplicate/stopped/invalid definitions fail without partial state.
    fn register_effect(
        &self,
        context: &Context,
        definition: LspServerDefinition,
    ) -> Result<(), LspError>;

    /// List registered safe server identities in stable id order.
    ///
    /// # Errors
    /// A stopped/unavailable provider fails rather than returning an empty set.
    fn servers(&self) -> Result<Vec<LspServerDescriptor>, LspError>;

    /// Resolve definitions.
    async fn definition(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError>;

    /// Resolve references.
    async fn references(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError>;

    /// Return hover/type information for one position.
    async fn hover(
        &self,
        _request: LspPositionRequest,
        _cancellation: CancellationToken,
    ) -> Result<Option<LspHover>, LspError> {
        Err(LspError::new(LspErrorCode::Unsupported))
    }

    /// Resolve interface/trait implementations.
    async fn implementations(
        &self,
        _request: LspPositionRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        Err(LspError::new(LspErrorCode::Unsupported))
    }

    /// List symbols in one document.
    async fn document_symbols(
        &self,
        _request: LspDocumentRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspSymbol>, LspError> {
        Err(LspError::new(LspErrorCode::Unsupported))
    }

    /// Search symbols across the server workspace.
    async fn workspace_symbols(
        &self,
        _request: LspWorkspaceSymbolRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspSymbol>, LspError> {
        Err(LspError::new(LspErrorCode::Unsupported))
    }

    /// Trace callers of the symbol at one position.
    async fn incoming_calls(
        &self,
        _request: LspPositionRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        Err(LspError::new(LspErrorCode::Unsupported))
    }

    /// Trace callees of the symbol at one position.
    async fn outgoing_calls(
        &self,
        _request: LspPositionRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        Err(LspError::new(LspErrorCode::Unsupported))
    }

    /// Read/synchronize a document and request pull diagnostics.
    async fn diagnostics(
        &self,
        request: LspDocumentRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspDiagnostic>, LspError>;

    /// Synchronously stop registrations/sessions and trigger process teardown.
    fn close(&self);
}

/// Typed wrapper around a replaceable LSP Provider.
#[derive(Clone)]
pub struct LspService {
    backend: Arc<dyn LspBackend>,
}

impl LspService {
    /// Bind a Provider.
    #[must_use]
    pub fn new(backend: Arc<dyn LspBackend>) -> Self {
        Self { backend }
    }

    /// Construct the local stdio Provider.
    #[must_use]
    pub fn local(subprocess: SubprocessService, filesystem: FileSystemService) -> Self {
        Self::new(Arc::new(LocalLspBackend::new(subprocess, filesystem)))
    }

    /// Register a stdio definition as an effect.
    ///
    /// # Errors
    /// Duplicate/stopped/invalid definitions fail without partial state.
    pub fn register_effect(
        &self,
        context: &Context,
        definition: LspServerDefinition,
    ) -> Result<(), LspError> {
        self.backend.register_effect(context, definition)
    }

    /// List registered safe server identities in stable id order.
    ///
    /// # Errors
    /// A stopped/unavailable provider fails rather than returning an empty set.
    pub fn servers(&self) -> Result<Vec<LspServerDescriptor>, LspError> {
        self.backend.servers()
    }

    /// Resolve definitions.
    ///
    /// # Errors
    /// Unknown server, cancellation, process, protocol and output validation
    /// failures are classified without provider bodies.
    pub async fn definition(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        self.backend.definition(request, cancellation).await
    }

    /// Resolve references.
    ///
    /// # Errors
    /// Same contract as [`Self::definition`].
    pub async fn references(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        self.backend.references(request, cancellation).await
    }

    /// Return hover/type information.
    ///
    /// # Errors
    /// Same classified provider boundary as [`Self::definition`].
    pub async fn hover(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Option<LspHover>, LspError> {
        self.backend.hover(request, cancellation).await
    }

    /// Resolve interface/trait implementations.
    ///
    /// # Errors
    /// Same classified provider boundary as [`Self::definition`].
    pub async fn implementations(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        self.backend.implementations(request, cancellation).await
    }

    /// List symbols in one document.
    ///
    /// # Errors
    /// Same classified provider boundary as [`Self::diagnostics`].
    pub async fn document_symbols(
        &self,
        request: LspDocumentRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspSymbol>, LspError> {
        self.backend.document_symbols(request, cancellation).await
    }

    /// Search symbols across the configured server workspace.
    ///
    /// # Errors
    /// Unknown server, cancellation, process, protocol and output validation
    /// failures remain body/path-free.
    pub async fn workspace_symbols(
        &self,
        request: LspWorkspaceSymbolRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspSymbol>, LspError> {
        self.backend.workspace_symbols(request, cancellation).await
    }

    /// Trace callers of the symbol at one position.
    ///
    /// # Errors
    /// Same classified provider boundary as [`Self::definition`].
    pub async fn incoming_calls(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        self.backend.incoming_calls(request, cancellation).await
    }

    /// Trace callees of the symbol at one position.
    ///
    /// # Errors
    /// Same classified provider boundary as [`Self::definition`].
    pub async fn outgoing_calls(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        self.backend.outgoing_calls(request, cancellation).await
    }

    /// Synchronize a bounded document and request diagnostics.
    ///
    /// # Errors
    /// Filesystem, server, cancellation, protocol and result validation
    /// failures are classified without provider bodies.
    pub async fn diagnostics(
        &self,
        request: LspDocumentRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        self.backend.diagnostics(request, cancellation).await
    }

    /// Stop this Provider generation and every server it owns.
    pub fn close(&self) {
        self.backend.close();
    }
}

impl std::fmt::Debug for LspService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("LspService").finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct Registration {
    token: String,
    definition: LspServerDefinition,
}

#[derive(Clone)]
struct LspSession {
    registration_token: String,
    definition: LspServerDefinition,
    cancellation: CancellationToken,
    connection: Arc<tokio::sync::Mutex<LspConnection>>,
}

struct RegistryState {
    open: bool,
    registrations: BTreeMap<LspServerId, Registration>,
    sessions: BTreeMap<LspServerId, LspSession>,
}

struct LocalLspInner {
    subprocess: SubprocessService,
    filesystem: FileSystemService,
    lifecycle: CancellationToken,
    start_gate: tokio::sync::Mutex<()>,
    state: Mutex<RegistryState>,
}

impl Drop for LocalLspInner {
    fn drop(&mut self) {
        self.lifecycle.cancel();
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        for session in state.sessions.values() {
            session.cancellation.cancel();
        }
        state.sessions.clear();
        state.registrations.clear();
        state.open = false;
    }
}

#[derive(Clone)]
struct LocalLspBackend {
    inner: Arc<LocalLspInner>,
}

impl LocalLspBackend {
    fn new(subprocess: SubprocessService, filesystem: FileSystemService) -> Self {
        Self {
            inner: Arc::new(LocalLspInner {
                subprocess,
                filesystem,
                lifecycle: CancellationToken::new(),
                start_gate: tokio::sync::Mutex::new(()),
                state: Mutex::new(RegistryState {
                    open: true,
                    registrations: BTreeMap::new(),
                    sessions: BTreeMap::new(),
                }),
            }),
        }
    }

    fn unregister(inner: &Weak<LocalLspInner>, id: &LspServerId, token: &str) {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let mut state = lock(&inner.state);
        if state
            .registrations
            .get(id)
            .is_some_and(|registration| registration.token == token)
        {
            state.registrations.remove(id);
            if let Some(session) = state.sessions.remove(id) {
                session.cancellation.cancel();
            }
        }
    }

    async fn session(
        &self,
        id: &LspServerId,
        caller: &CancellationToken,
    ) -> Result<LspSession, LspError> {
        check_operation(caller, &self.inner.lifecycle)?;
        if let Some(session) = lock(&self.inner.state).sessions.get(id).cloned() {
            return Ok(session);
        }
        let _gate = tokio::select! {
            _ = caller.cancelled() => return Err(LspError::new(LspErrorCode::Cancelled)),
            _ = self.inner.lifecycle.cancelled() => {
                return Err(LspError::new(LspErrorCode::ServiceStopped));
            }
            gate = self.inner.start_gate.lock() => gate,
        };
        let registration = {
            let state = lock(&self.inner.state);
            ensure_registry_open(&state)?;
            if let Some(session) = state.sessions.get(id).cloned() {
                return Ok(session);
            }
            state
                .registrations
                .get(id)
                .cloned()
                .ok_or_else(|| LspError::new(LspErrorCode::UnknownServer))?
        };
        check_operation(caller, &self.inner.lifecycle)?;
        let session_cancellation = self.inner.lifecycle.child_token();
        let relay_target = session_cancellation.clone();
        let relay_caller = caller.clone();
        let relay = tokio::spawn(async move {
            relay_caller.cancelled().await;
            relay_target.cancel();
        });
        let started = self
            .inner
            .subprocess
            .spawn_interactive_raw(
                registration.definition.process.clone(),
                session_cancellation.clone(),
            )
            .await
            .map_err(map_process_error);
        let raw = match started {
            Ok(raw) => raw,
            Err(error) => {
                relay.abort();
                let _ = relay.await;
                return Err(error);
            }
        };
        let (process, input, output) = raw.into_raw_parts();
        let mut connection = LspConnection::new(process, input, output);
        let initialized = connection
            .initialize(&registration.definition, caller, &session_cancellation)
            .await;
        relay.abort();
        let _ = relay.await;
        initialized?;
        if caller.is_cancelled() {
            session_cancellation.cancel();
            return Err(LspError::new(LspErrorCode::Cancelled));
        }
        if self.inner.lifecycle.is_cancelled() {
            session_cancellation.cancel();
            return Err(LspError::new(LspErrorCode::ServiceStopped));
        }
        let session = LspSession {
            registration_token: registration.token.clone(),
            definition: registration.definition.clone(),
            cancellation: session_cancellation,
            connection: Arc::new(tokio::sync::Mutex::new(connection)),
        };
        let mut state = lock(&self.inner.state);
        ensure_registry_open(&state)?;
        if state
            .registrations
            .get(id)
            .is_none_or(|current| current.token != registration.token)
        {
            session.cancellation.cancel();
            return Err(LspError::new(LspErrorCode::UnknownServer));
        }
        state.sessions.insert(id.clone(), session.clone());
        Ok(session)
    }

    async fn request_value(
        &self,
        session: &LspSession,
        method: &'static str,
        params: serde_json::Value,
        caller: &CancellationToken,
    ) -> Result<serde_json::Value, LspError> {
        check_operation(caller, &session.cancellation)?;
        let mut connection = tokio::select! {
            _ = caller.cancelled() => return Err(LspError::new(LspErrorCode::Cancelled)),
            _ = session.cancellation.cancelled() => {
                return Err(self.session_stopped_error());
            }
            connection = session.connection.lock() => connection,
        };
        connection
            .call(method, params, caller, &session.cancellation)
            .await
    }

    async fn request_document_value(
        &self,
        session: &LspSession,
        path: &ResolvedPath,
        method: &'static str,
        build_params: impl FnOnce(&str) -> serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, LspError> {
        let output = self
            .inner
            .filesystem
            .read(
                ReadFileSpec::new(path.clone(), MAX_LSP_DOCUMENT_BYTES)
                    .map_err(|_| LspError::new(LspErrorCode::InvalidSpec))?,
                cancellation.clone(),
            )
            .await
            .map_err(|error| match error.code() {
                crate::FileSystemErrorCode::Cancelled => LspError::new(LspErrorCode::Cancelled),
                crate::FileSystemErrorCode::InvalidOutput => {
                    LspError::new(LspErrorCode::OutputLimit)
                }
                _ => LspError::new(LspErrorCode::Io),
            })?;
        if output.truncated() {
            return Err(LspError::new(LspErrorCode::OutputLimit));
        }
        let text = std::str::from_utf8(output.bytes())
            .map_err(|_| LspError::new(LspErrorCode::InvalidSpec))?;
        let uri = file_uri(path.as_path())?;
        let params = build_params(&uri);
        let mut connection = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(LspError::new(LspErrorCode::Cancelled));
            }
            _ = session.cancellation.cancelled() => {
                return Err(self.session_stopped_error());
            }
            connection = session.connection.lock() => connection,
        };
        connection
            .synchronize_document(
                &uri,
                session.definition.language_id(),
                text,
                cancellation,
                &session.cancellation,
            )
            .await?;
        if method == "textDocument/diagnostic" {
            connection
                .diagnostics_value(&uri, params, cancellation, &session.cancellation)
                .await
        } else {
            connection
                .call(method, params, cancellation, &session.cancellation)
                .await
        }
    }

    fn session_stopped_error(&self) -> LspError {
        if self.inner.lifecycle.is_cancelled() {
            LspError::new(LspErrorCode::ServiceStopped)
        } else {
            LspError::new(LspErrorCode::UnknownServer)
        }
    }

    fn validate_request_path(
        definition: &LspServerDefinition,
        path: &ResolvedPath,
    ) -> Result<(), LspError> {
        if path.as_path().starts_with(definition.workspace.as_path()) {
            Ok(())
        } else {
            Err(LspError::new(LspErrorCode::InvalidSpec))
        }
    }

    fn retire_failed_session(&self, id: &LspServerId, session: &LspSession, error: &LspError) {
        if error.code == LspErrorCode::Cancelled {
            return;
        }
        let mut state = lock(&self.inner.state);
        if state
            .sessions
            .get(id)
            .is_some_and(|current| current.registration_token == session.registration_token)
        {
            state.sessions.remove(id);
            session.cancellation.cancel();
        }
    }

    async fn call_hierarchy(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
        direction: CallDirection,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        let session = self.session(&request.server, &cancellation).await?;
        Self::validate_request_path(&session.definition, &request.path)?;
        let prepared = self
            .request_document_value(
                &session,
                &request.path,
                "textDocument/prepareCallHierarchy",
                |uri| position_params(uri, request.position),
                &cancellation,
            )
            .await;
        let result = async {
            let items = parse_call_hierarchy_items(prepared?)?;
            let mut edges = Vec::new();
            let mut call_ranges = 0_usize;
            for item in items {
                let value = self
                    .request_value(
                        &session,
                        direction.method(),
                        serde_json::json!({"item":item}),
                        &cancellation,
                    )
                    .await?;
                parse_call_edges(
                    &self.inner.filesystem,
                    &session.definition.workspace,
                    direction,
                    value,
                    &mut edges,
                    &mut call_ranges,
                )?;
                if edges.len() > MAX_LSP_CALLS {
                    return Err(LspError::new(LspErrorCode::OutputLimit));
                }
            }
            edges.sort_by(|left, right| {
                (
                    left.symbol.path.as_path(),
                    left.symbol.range,
                    &left.symbol.name,
                )
                    .cmp(&(
                        right.symbol.path.as_path(),
                        right.symbol.range,
                        &right.symbol.name,
                    ))
            });
            Ok(edges)
        }
        .await;
        if let Err(error) = &result {
            self.retire_failed_session(&request.server, &session, error);
        }
        result
    }
}

#[derive(Clone, Copy)]
enum CallDirection {
    Incoming,
    Outgoing,
}

impl CallDirection {
    const fn method(self) -> &'static str {
        match self {
            Self::Incoming => "callHierarchy/incomingCalls",
            Self::Outgoing => "callHierarchy/outgoingCalls",
        }
    }

    const fn item_key(self) -> &'static str {
        match self {
            Self::Incoming => "from",
            Self::Outgoing => "to",
        }
    }
}

#[async_trait]
impl LspBackend for LocalLspBackend {
    fn register_effect(
        &self,
        context: &Context,
        definition: LspServerDefinition,
    ) -> Result<(), LspError> {
        let resolved = self
            .inner
            .filesystem
            .resolve(
                PathRequest::new(
                    definition.workspace.as_path(),
                    definition.workspace.as_path(),
                )
                .map_err(|_| LspError::new(LspErrorCode::InvalidSpec))?,
            )
            .map_err(|_| LspError::new(LspErrorCode::InvalidSpec))?;
        if resolved != definition.workspace {
            return Err(LspError::new(LspErrorCode::InvalidSpec));
        }
        let id = definition.id.clone();
        let token = uuid::Uuid::new_v4().to_string();
        {
            let mut state = lock(&self.inner.state);
            ensure_registry_open(&state)?;
            if state.registrations.contains_key(&id) {
                return Err(LspError::new(LspErrorCode::DuplicateServer));
            }
            state.registrations.insert(
                id.clone(),
                Registration {
                    token: token.clone(),
                    definition,
                },
            );
        }
        let weak = Arc::downgrade(&self.inner);
        context.effect(move || Self::unregister(&weak, &id, &token));
        Ok(())
    }

    fn servers(&self) -> Result<Vec<LspServerDescriptor>, LspError> {
        let state = lock(&self.inner.state);
        ensure_registry_open(&state)?;
        Ok(state
            .registrations
            .values()
            .map(|registration| LspServerDescriptor {
                id: registration.definition.id.clone(),
                language_id: registration.definition.language_id.clone(),
            })
            .collect())
    }

    async fn definition(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        let session = self.session(&request.server, &cancellation).await?;
        Self::validate_request_path(&session.definition, &request.path)?;
        let value = self
            .request_document_value(
                &session,
                &request.path,
                "textDocument/definition",
                |uri| position_params(uri, request.position),
                &cancellation,
            )
            .await;
        let result = match value {
            Ok(value) => {
                parse_locations(&self.inner.filesystem, &session.definition.workspace, value)
            }
            Err(error) => Err(error),
        };
        if let Err(error) = &result {
            self.retire_failed_session(&request.server, &session, error);
        }
        result
    }

    async fn references(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        let session = self.session(&request.server, &cancellation).await?;
        Self::validate_request_path(&session.definition, &request.path)?;
        let value = self
            .request_document_value(
                &session,
                &request.path,
                "textDocument/references",
                |uri| {
                    serde_json::json!({
                        "textDocument":{"uri":uri},
                        "position":position_value(request.position),
                        "context":{"includeDeclaration":true}
                    })
                },
                &cancellation,
            )
            .await;
        let result = match value {
            Ok(value) => {
                parse_locations(&self.inner.filesystem, &session.definition.workspace, value)
            }
            Err(error) => Err(error),
        };
        if let Err(error) = &result {
            self.retire_failed_session(&request.server, &session, error);
        }
        result
    }

    async fn hover(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Option<LspHover>, LspError> {
        let session = self.session(&request.server, &cancellation).await?;
        Self::validate_request_path(&session.definition, &request.path)?;
        let value = self
            .request_document_value(
                &session,
                &request.path,
                "textDocument/hover",
                |uri| position_params(uri, request.position),
                &cancellation,
            )
            .await;
        let result = value.and_then(parse_hover);
        if let Err(error) = &result {
            self.retire_failed_session(&request.server, &session, error);
        }
        result
    }

    async fn implementations(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        let session = self.session(&request.server, &cancellation).await?;
        Self::validate_request_path(&session.definition, &request.path)?;
        let value = self
            .request_document_value(
                &session,
                &request.path,
                "textDocument/implementation",
                |uri| position_params(uri, request.position),
                &cancellation,
            )
            .await;
        let result = value.and_then(|value| {
            parse_locations(&self.inner.filesystem, &session.definition.workspace, value)
        });
        if let Err(error) = &result {
            self.retire_failed_session(&request.server, &session, error);
        }
        result
    }

    async fn document_symbols(
        &self,
        request: LspDocumentRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspSymbol>, LspError> {
        let session = self.session(&request.server, &cancellation).await?;
        Self::validate_request_path(&session.definition, &request.path)?;
        let value = self
            .request_document_value(
                &session,
                &request.path,
                "textDocument/documentSymbol",
                |uri| serde_json::json!({"textDocument":{"uri":uri}}),
                &cancellation,
            )
            .await;
        let result = value.and_then(|value| {
            parse_document_symbols(
                &self.inner.filesystem,
                &session.definition.workspace,
                request.path,
                value,
            )
        });
        if let Err(error) = &result {
            self.retire_failed_session(&request.server, &session, error);
        }
        result
    }

    async fn workspace_symbols(
        &self,
        request: LspWorkspaceSymbolRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspSymbol>, LspError> {
        let session = self.session(&request.server, &cancellation).await?;
        let value = self
            .request_value(
                &session,
                "workspace/symbol",
                serde_json::json!({"query":request.query}),
                &cancellation,
            )
            .await;
        let result = value.and_then(|value| {
            parse_workspace_symbols(&self.inner.filesystem, &session.definition.workspace, value)
        });
        if let Err(error) = &result {
            self.retire_failed_session(&request.server, &session, error);
        }
        result
    }

    async fn incoming_calls(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        self.call_hierarchy(request, cancellation, CallDirection::Incoming)
            .await
    }

    async fn outgoing_calls(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        self.call_hierarchy(request, cancellation, CallDirection::Outgoing)
            .await
    }

    async fn diagnostics(
        &self,
        request: LspDocumentRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let session = self.session(&request.server, &cancellation).await?;
        Self::validate_request_path(&session.definition, &request.path)?;
        let value = self
            .request_document_value(
                &session,
                &request.path,
                "textDocument/diagnostic",
                |uri| serde_json::json!({"textDocument":{"uri":uri}}),
                &cancellation,
            )
            .await;
        let result = match value {
            Ok(value) => parse_diagnostics(request.path, value),
            Err(error) => Err(error),
        };
        if let Err(error) = &result {
            self.retire_failed_session(&request.server, &session, error);
        }
        result
    }

    fn close(&self) {
        let mut state = lock(&self.inner.state);
        if !state.open {
            return;
        }
        state.open = false;
        self.inner.lifecycle.cancel();
        for session in state.sessions.values() {
            session.cancellation.cancel();
        }
        state.sessions.clear();
        state.registrations.clear();
    }
}

fn ensure_registry_open(state: &RegistryState) -> Result<(), LspError> {
    if state.open {
        Ok(())
    } else {
        Err(LspError::new(LspErrorCode::ServiceStopped))
    }
}

fn check_operation(
    caller: &CancellationToken,
    lifecycle: &CancellationToken,
) -> Result<(), LspError> {
    if lifecycle.is_cancelled() {
        Err(LspError::new(LspErrorCode::ServiceStopped))
    } else if caller.is_cancelled() {
        Err(LspError::new(LspErrorCode::Cancelled))
    } else {
        Ok(())
    }
}

struct LspConnection {
    _process: ManagedProcess,
    input: ProcessInput,
    output: ProcessOutputReader,
    framer: LspFramer,
    next_id: u64,
    document_versions: BTreeMap<String, i32>,
    pull_diagnostics: bool,
    published_diagnostics: BTreeMap<String, serde_json::Value>,
}

impl LspConnection {
    fn new(process: ManagedProcess, input: ProcessInput, output: ProcessOutputReader) -> Self {
        Self {
            _process: process,
            input,
            output,
            framer: LspFramer::default(),
            next_id: 1,
            document_versions: BTreeMap::new(),
            pull_diagnostics: false,
            published_diagnostics: BTreeMap::new(),
        }
    }

    async fn initialize(
        &mut self,
        definition: &LspServerDefinition,
        caller: &CancellationToken,
        lifecycle: &CancellationToken,
    ) -> Result<(), LspError> {
        let root_uri = file_uri(definition.workspace.as_path())?;
        let result = self
            .call(
                "initialize",
                serde_json::json!({
                    "processId":serde_json::Value::Null,
                    "clientInfo":{"name":"heycode","version":env!("CARGO_PKG_VERSION")},
                    "rootUri":root_uri,
                    "capabilities":{
                        "textDocument":{"diagnostic":{"dynamicRegistration":false}},
                        "workspace":{"workspaceFolders":false}
                    }
                }),
                caller,
                lifecycle,
            )
            .await?;
        let capabilities = result
            .as_object()
            .and_then(|object| object.get("capabilities"))
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        self.pull_diagnostics = capabilities
            .get("diagnosticProvider")
            .is_some_and(|value| !value.is_null() && value.as_bool() != Some(false));
        self.notify("initialized", serde_json::json!({})).await
    }

    async fn diagnostics_value(
        &mut self,
        uri: &str,
        pull_params: serde_json::Value,
        caller: &CancellationToken,
        lifecycle: &CancellationToken,
    ) -> Result<serde_json::Value, LspError> {
        if self.pull_diagnostics {
            return self
                .call("textDocument/diagnostic", pull_params, caller, lifecycle)
                .await;
        }
        if let Some(items) = self.published_diagnostics.remove(uri) {
            return Ok(serde_json::json!({"kind":"full", "items":items}));
        }
        let deadline = tokio::time::sleep(Duration::from_millis(PUSH_DIAGNOSTICS_WAIT_MS));
        tokio::pin!(deadline);
        loop {
            let message = tokio::select! {
                () = caller.cancelled() => return Err(LspError::new(LspErrorCode::Cancelled)),
                () = lifecycle.cancelled() => return Err(LspError::new(LspErrorCode::ServiceStopped)),
                () = &mut deadline => return Err(LspError::new(LspErrorCode::Unsupported)),
                message = self.next_message(caller, lifecycle) => message?,
            };
            validate_jsonrpc(&message)?;
            if message.get("method").is_some() {
                self.handle_server_message(&message).await?;
            }
            if let Some(items) = self.published_diagnostics.remove(uri) {
                return Ok(serde_json::json!({"kind":"full", "items":items}));
            }
        }
    }

    async fn synchronize_document(
        &mut self,
        uri: &str,
        language_id: &str,
        text: &str,
        caller: &CancellationToken,
        lifecycle: &CancellationToken,
    ) -> Result<(), LspError> {
        check_operation(caller, lifecycle)?;
        let next_version = self
            .document_versions
            .get(uri)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| LspError::new(LspErrorCode::OutputLimit))?;
        let params = if next_version == 1 {
            serde_json::json!({
                "textDocument":{
                    "uri":uri,"languageId":language_id,"version":next_version,"text":text
                }
            })
        } else {
            serde_json::json!({
                "textDocument":{"uri":uri,"version":next_version},
                "contentChanges":[{"text":text}]
            })
        };
        let method = if next_version == 1 {
            "textDocument/didOpen"
        } else {
            "textDocument/didChange"
        };
        self.notify(method, params).await?;
        self.document_versions.insert(uri.to_owned(), next_version);
        Ok(())
    }

    async fn call(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        caller: &CancellationToken,
        lifecycle: &CancellationToken,
    ) -> Result<serde_json::Value, LspError> {
        check_operation(caller, lifecycle)?;
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| LspError::new(LspErrorCode::OutputLimit))?;
        self.write_message(&serde_json::json!({
            "jsonrpc":"2.0","id":id,"method":method,"params":params
        }))
        .await?;
        loop {
            let message = match self.next_message(caller, lifecycle).await {
                Ok(message) => message,
                Err(error) if error.code == LspErrorCode::Cancelled => {
                    let _ = self
                        .notify("$/cancelRequest", serde_json::json!({"id":id}))
                        .await;
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
            validate_jsonrpc(&message)?;
            if message.get("method").is_some() {
                self.handle_server_message(&message).await?;
                continue;
            }
            if message.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
                continue;
            }
            if message.get("error").is_some() {
                return Err(LspError::new(LspErrorCode::Protocol));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| LspError::new(LspErrorCode::Protocol));
        }
    }

    async fn notify(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<(), LspError> {
        self.write_message(&serde_json::json!({
            "jsonrpc":"2.0","method":method,"params":params
        }))
        .await
    }

    async fn handle_server_message(&mut self, message: &serde_json::Value) -> Result<(), LspError> {
        if message.get("method").and_then(serde_json::Value::as_str)
            == Some("textDocument/publishDiagnostics")
        {
            let params = message
                .get("params")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
            let uri = params
                .get("uri")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= MAX_LSP_FRAME_BYTES)
                .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
            let diagnostics = params
                .get("diagnostics")
                .and_then(serde_json::Value::as_array)
                .filter(|rows| rows.len() <= MAX_LSP_DIAGNOSTICS)
                .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
            self.published_diagnostics.insert(
                uri.to_owned(),
                serde_json::Value::Array(diagnostics.clone()),
            );
            return Ok(());
        }
        let Some(id) = message.get("id").cloned() else {
            return Ok(());
        };
        self.write_message(&serde_json::json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32601,"message":"method not supported by heycode"}
        }))
        .await
    }

    async fn write_message(&mut self, message: &serde_json::Value) -> Result<(), LspError> {
        let body =
            serde_json::to_vec(message).map_err(|_| LspError::new(LspErrorCode::Protocol))?;
        if body.is_empty() || body.len() > MAX_LSP_FRAME_BYTES {
            return Err(LspError::new(LspErrorCode::OutputLimit));
        }
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        self.input.write(&frame).await.map_err(map_process_error)
    }

    async fn next_message(
        &mut self,
        caller: &CancellationToken,
        lifecycle: &CancellationToken,
    ) -> Result<serde_json::Value, LspError> {
        loop {
            if let Some(message) = self.framer.next()? {
                return Ok(message);
            }
            let operation = CancellationToken::new();
            let relay_operation = operation.clone();
            let relay_caller = caller.clone();
            let relay_lifecycle = lifecycle.clone();
            let relay = tokio::spawn(async move {
                tokio::select! {
                    _ = relay_caller.cancelled() => relay_operation.cancel(),
                    _ = relay_lifecycle.cancelled() => relay_operation.cancel(),
                }
            });
            let chunk = self.output.read_chunk(operation).await;
            relay.abort();
            let _ = relay.await;
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) if error.code() == ProcessErrorCode::Cancelled => {
                    if lifecycle.is_cancelled() {
                        return Err(LspError::new(LspErrorCode::ServiceStopped));
                    }
                    return Err(LspError::new(LspErrorCode::Cancelled));
                }
                Err(error) => return Err(map_process_error(error)),
            };
            match chunk {
                ProcessOutputChunk::Data(bytes) => self.framer.push(&bytes)?,
                ProcessOutputChunk::Eof => return Err(LspError::new(LspErrorCode::Protocol)),
            }
        }
    }
}

#[derive(Default)]
struct LspFramer {
    buffer: Vec<u8>,
}

impl LspFramer {
    fn push(&mut self, bytes: &[u8]) -> Result<(), LspError> {
        if self
            .buffer
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > MAX_LSP_FRAME_BYTES + MAX_LSP_HEADER_BYTES)
        {
            return Err(LspError::new(LspErrorCode::OutputLimit));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn next(&mut self) -> Result<Option<serde_json::Value>, LspError> {
        let marker = b"Content-Length:";
        let Some(start) = find_bytes(&self.buffer, marker) else {
            if self.buffer.len() > MAX_LSP_HEADER_BYTES {
                return Err(LspError::new(LspErrorCode::Protocol));
            }
            return Ok(None);
        };
        if start > 0 {
            self.buffer.drain(..start);
        }
        let Some(header_end) = find_bytes(&self.buffer, b"\r\n\r\n") else {
            if self.buffer.len() > MAX_LSP_HEADER_BYTES {
                return Err(LspError::new(LspErrorCode::Protocol));
            }
            return Ok(None);
        };
        if header_end > MAX_LSP_HEADER_BYTES {
            return Err(LspError::new(LspErrorCode::Protocol));
        }
        let header = std::str::from_utf8(&self.buffer[..header_end])
            .map_err(|_| LspError::new(LspErrorCode::Protocol))?;
        let lengths = header.lines().filter_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
        });
        let mut lengths = lengths.map(str::parse::<usize>);
        let length = lengths
            .next()
            .transpose()
            .map_err(|_| LspError::new(LspErrorCode::Protocol))?
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        if lengths.next().is_some() || length == 0 || length > MAX_LSP_FRAME_BYTES {
            return Err(LspError::new(LspErrorCode::OutputLimit));
        }
        let body_start = header_end + 4;
        let frame_end = body_start
            .checked_add(length)
            .ok_or_else(|| LspError::new(LspErrorCode::OutputLimit))?;
        if self.buffer.len() < frame_end {
            return Ok(None);
        }
        let body = self.buffer[body_start..frame_end].to_vec();
        self.buffer.drain(..frame_end);
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|_| LspError::new(LspErrorCode::Protocol))
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn validate_jsonrpc(message: &serde_json::Value) -> Result<(), LspError> {
    let object = message
        .as_object()
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) == Some("2.0") {
        Ok(())
    } else {
        Err(LspError::new(LspErrorCode::Protocol))
    }
}

fn position_value(position: LspPosition) -> serde_json::Value {
    serde_json::json!({"line":position.line,"character":position.character})
}

fn position_params(uri: &str, position: LspPosition) -> serde_json::Value {
    serde_json::json!({
        "textDocument":{"uri":uri},
        "position":position_value(position)
    })
}

fn file_uri(path: &Path) -> Result<String, LspError> {
    Url::from_file_path(path)
        .map(String::from)
        .map_err(|_| LspError::new(LspErrorCode::InvalidSpec))
}

fn parse_locations(
    filesystem: &FileSystemService,
    workspace: &ResolvedPath,
    value: serde_json::Value,
) -> Result<Vec<LspLocation>, LspError> {
    let values = match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(values) => values,
        value @ serde_json::Value::Object(_) => vec![value],
        _ => return Err(LspError::new(LspErrorCode::Protocol)),
    };
    if values.len() > MAX_LSP_LOCATIONS {
        return Err(LspError::new(LspErrorCode::OutputLimit));
    }
    let mut locations = Vec::with_capacity(values.len());
    for value in values {
        let object = value
            .as_object()
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let uri = object
            .get("uri")
            .or_else(|| object.get("targetUri"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let range = object
            .get("range")
            .or_else(|| object.get("targetSelectionRange"))
            .or_else(|| object.get("targetRange"))
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let path = resolve_uri(filesystem, workspace, uri)?;
        locations.push(LspLocation {
            path,
            range: parse_range(range)?,
        });
    }
    locations.sort_by(|left, right| {
        (left.path.as_path(), left.range).cmp(&(right.path.as_path(), right.range))
    });
    locations.dedup_by(|left, right| left.path == right.path && left.range == right.range);
    Ok(locations)
}

fn parse_hover(value: serde_json::Value) -> Result<Option<LspHover>, LspError> {
    let Some(object) = value.as_object() else {
        return if value.is_null() {
            Ok(None)
        } else {
            Err(LspError::new(LspErrorCode::Protocol))
        };
    };
    let contents = object
        .get("contents")
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    let mut parts = Vec::new();
    match contents {
        serde_json::Value::String(text) => parts.push(text.as_str()),
        serde_json::Value::Object(row) => parts.push(markup_text(row)?),
        serde_json::Value::Array(rows) => {
            if rows.len() > MAX_LSP_SYMBOLS {
                return Err(LspError::new(LspErrorCode::OutputLimit));
            }
            for row in rows {
                match row {
                    serde_json::Value::String(text) => parts.push(text.as_str()),
                    serde_json::Value::Object(row) => parts.push(markup_text(row)?),
                    _ => return Err(LspError::new(LspErrorCode::Protocol)),
                }
            }
        }
        _ => return Err(LspError::new(LspErrorCode::Protocol)),
    }
    let text = parts.join("\n\n");
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > MAX_LSP_HOVER_BYTES {
        return Err(LspError::new(LspErrorCode::OutputLimit));
    }
    if contains_unsafe_control(&text) {
        return Err(LspError::new(LspErrorCode::Protocol));
    }
    let range = object.get("range").map(parse_range).transpose()?;
    LspHover::new(text, range)
        .map(Some)
        .map_err(|_| LspError::new(LspErrorCode::Protocol))
}

fn markup_text(object: &serde_json::Map<String, serde_json::Value>) -> Result<&str, LspError> {
    object
        .get("value")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))
}

fn parse_document_symbols(
    filesystem: &FileSystemService,
    workspace: &ResolvedPath,
    path: ResolvedPath,
    value: serde_json::Value,
) -> Result<Vec<LspSymbol>, LspError> {
    let rows = nullable_array(value)?;
    let mut symbols = Vec::new();
    for row in rows {
        parse_symbol_tree(filesystem, workspace, Some(&path), &row, None, &mut symbols)?;
    }
    sort_symbols(&mut symbols);
    Ok(symbols)
}

fn parse_workspace_symbols(
    filesystem: &FileSystemService,
    workspace: &ResolvedPath,
    value: serde_json::Value,
) -> Result<Vec<LspSymbol>, LspError> {
    let rows = nullable_array(value)?;
    let mut symbols = Vec::new();
    for row in rows {
        parse_symbol_tree(filesystem, workspace, None, &row, None, &mut symbols)?;
    }
    sort_symbols(&mut symbols);
    Ok(symbols)
}

fn nullable_array(value: serde_json::Value) -> Result<Vec<serde_json::Value>, LspError> {
    match value {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Array(rows) if rows.len() <= MAX_LSP_SYMBOLS => Ok(rows),
        serde_json::Value::Array(_) => Err(LspError::new(LspErrorCode::OutputLimit)),
        _ => Err(LspError::new(LspErrorCode::Protocol)),
    }
}

fn parse_symbol_tree(
    filesystem: &FileSystemService,
    workspace: &ResolvedPath,
    default_path: Option<&ResolvedPath>,
    value: &serde_json::Value,
    inherited_container: Option<&str>,
    symbols: &mut Vec<LspSymbol>,
) -> Result<(), LspError> {
    if symbols.len() >= MAX_LSP_SYMBOLS {
        return Err(LspError::new(LspErrorCode::OutputLimit));
    }
    let object = value
        .as_object()
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    let name = bounded_field(object, "name")?;
    let detail = optional_bounded_field(object, "detail")?;
    let kind = object
        .get("kind")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    let explicit_container = optional_bounded_field(object, "containerName")?;
    let container_name = explicit_container.or_else(|| inherited_container.map(str::to_owned));
    let (path, range, selection_range) = if let Some(location) = object.get("location") {
        let location = location
            .as_object()
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let uri = location
            .get("uri")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let range = match location.get("range") {
            Some(range) => parse_range(range)?,
            // LSP 3.17 permits an unresolved WorkspaceSymbol location to
            // carry only its URI. SymbolInformation returned for a document
            // still requires a concrete range.
            None if default_path.is_none() => {
                LspRange::new(LspPosition::new(0, 0), LspPosition::new(0, 0))?
            }
            None => return Err(LspError::new(LspErrorCode::Protocol)),
        };
        (resolve_uri(filesystem, workspace, uri)?, range, range)
    } else {
        let path = default_path
            .cloned()
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let range = parse_range(
            object
                .get("range")
                .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?,
        )?;
        let selection_range = object
            .get("selectionRange")
            .map(parse_range)
            .transpose()?
            .unwrap_or(range);
        (path, range, selection_range)
    };
    symbols.push(
        LspSymbol::new(
            name.clone(),
            detail,
            kind,
            path,
            range,
            selection_range,
            container_name,
        )
        .map_err(|_| LspError::new(LspErrorCode::Protocol))?,
    );
    if let Some(children) = object.get("children") {
        let children = children
            .as_array()
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        for child in children {
            parse_symbol_tree(
                filesystem,
                workspace,
                default_path,
                child,
                Some(&name),
                symbols,
            )?;
        }
    }
    Ok(())
}

fn parse_call_hierarchy_items(
    value: serde_json::Value,
) -> Result<Vec<serde_json::Value>, LspError> {
    let items = nullable_array(value)?;
    if items.len() > MAX_LSP_PREPARED_CALL_ITEMS {
        return Err(LspError::new(LspErrorCode::OutputLimit));
    }
    if items.iter().any(|item| !item.is_object()) {
        return Err(LspError::new(LspErrorCode::Protocol));
    }
    Ok(items)
}

fn parse_call_edges(
    filesystem: &FileSystemService,
    workspace: &ResolvedPath,
    direction: CallDirection,
    value: serde_json::Value,
    edges: &mut Vec<LspCallHierarchyEdge>,
    call_range_count: &mut usize,
) -> Result<(), LspError> {
    for row in nullable_array(value)? {
        let object = row
            .as_object()
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let item = object
            .get(direction.item_key())
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let symbol = parse_call_symbol(filesystem, workspace, item)?;
        let ranges = object
            .get("fromRanges")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        if edges.len() >= MAX_LSP_CALLS
            || ranges.len() > MAX_LSP_CALLS
            || call_range_count.saturating_add(ranges.len()) > MAX_LSP_CALLS
        {
            return Err(LspError::new(LspErrorCode::OutputLimit));
        }
        let call_ranges = ranges.iter().map(parse_range).collect::<Result<_, _>>()?;
        *call_range_count = call_range_count.saturating_add(ranges.len());
        edges.push(LspCallHierarchyEdge::new(symbol, call_ranges));
    }
    Ok(())
}

fn parse_call_symbol(
    filesystem: &FileSystemService,
    workspace: &ResolvedPath,
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<LspSymbol, LspError> {
    let name = bounded_field(object, "name")?;
    let detail = optional_bounded_field(object, "detail")?;
    let kind = object
        .get("kind")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    let uri = object
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    let range = parse_range(
        object
            .get("range")
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?,
    )?;
    let selection_range = parse_range(
        object
            .get("selectionRange")
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?,
    )?;
    LspSymbol::new(
        name,
        detail,
        kind,
        resolve_uri(filesystem, workspace, uri)?,
        range,
        selection_range,
        None,
    )
    .map_err(|_| LspError::new(LspErrorCode::Protocol))
}

fn bounded_field(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<String, LspError> {
    object
        .get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|value| valid_lsp_text(value))
        .map(str::to_owned)
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))
}

fn optional_bounded_field(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<Option<String>, LspError> {
    let Some(value) = object.get(name).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    if value.is_empty() {
        return Ok(None);
    }
    valid_lsp_text(value)
        .then(|| Some(value.to_owned()))
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))
}

fn sort_symbols(symbols: &mut [LspSymbol]) {
    symbols.sort_by(|left, right| {
        (left.path.as_path(), left.range, &left.name).cmp(&(
            right.path.as_path(),
            right.range,
            &right.name,
        ))
    });
}

fn resolve_uri(
    filesystem: &FileSystemService,
    workspace: &ResolvedPath,
    uri: &str,
) -> Result<ResolvedPath, LspError> {
    let url = Url::parse(uri).map_err(|_| LspError::new(LspErrorCode::Protocol))?;
    if url.scheme() != "file" || url.query().is_some() || url.fragment().is_some() {
        return Err(LspError::new(LspErrorCode::Protocol));
    }
    let path = url
        .to_file_path()
        .map_err(|_| LspError::new(LspErrorCode::Protocol))?;
    let resolved = filesystem
        .resolve(
            PathRequest::new(workspace.as_path(), path)
                .map_err(|_| LspError::new(LspErrorCode::Protocol))?,
        )
        .map_err(|_| LspError::new(LspErrorCode::Protocol))?;
    if resolved.as_path().starts_with(workspace.as_path()) {
        Ok(resolved)
    } else {
        Err(LspError::new(LspErrorCode::Protocol))
    }
}

fn parse_range(value: &serde_json::Value) -> Result<LspRange, LspError> {
    let object = value
        .as_object()
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    LspRange::new(
        parse_position(
            object
                .get("start")
                .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?,
        )?,
        parse_position(
            object
                .get("end")
                .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?,
        )?,
    )
    .map_err(|_| LspError::new(LspErrorCode::Protocol))
}

fn parse_position(value: &serde_json::Value) -> Result<LspPosition, LspError> {
    let object = value
        .as_object()
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    let line = object
        .get("line")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    let character = object
        .get("character")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    Ok(LspPosition::new(line, character))
}

fn parse_diagnostics(
    path: ResolvedPath,
    value: serde_json::Value,
) -> Result<Vec<LspDiagnostic>, LspError> {
    let items = value
        .as_object()
        .and_then(|object| object.get("items"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
    if items.len() > MAX_LSP_DIAGNOSTICS {
        return Err(LspError::new(LspErrorCode::OutputLimit));
    }
    let mut diagnostics = Vec::with_capacity(items.len());
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?;
        let message = object
            .get("message")
            .and_then(serde_json::Value::as_str)
            .filter(|message| valid_lsp_text(message))
            .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?
            .to_owned();
        let source = object
            .get("source")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .filter(|source| valid_lsp_text(source))
                    .map(str::to_owned)
                    .ok_or_else(|| LspError::new(LspErrorCode::Protocol))
            })
            .transpose()?;
        diagnostics.push(LspDiagnostic {
            path: path.clone(),
            range: parse_range(
                object
                    .get("range")
                    .ok_or_else(|| LspError::new(LspErrorCode::Protocol))?,
            )?,
            severity: match object.get("severity").and_then(serde_json::Value::as_u64) {
                Some(1) => LspDiagnosticSeverity::Error,
                Some(2) => LspDiagnosticSeverity::Warning,
                Some(3) => LspDiagnosticSeverity::Information,
                Some(4) => LspDiagnosticSeverity::Hint,
                Some(_) | None => LspDiagnosticSeverity::Unknown,
            },
            message,
            source,
        });
    }
    diagnostics.sort_by(|left, right| {
        (left.range, left.severity, &left.message).cmp(&(
            right.range,
            right.severity,
            &right.message,
        ))
    });
    Ok(diagnostics)
}

fn valid_lsp_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_LSP_TEXT_BYTES && !contains_unsafe_control(value)
}

fn contains_unsafe_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

fn map_process_error(error: ProcessError) -> LspError {
    let code = match error.code() {
        ProcessErrorCode::Cancelled => LspErrorCode::Cancelled,
        ProcessErrorCode::ServiceStopped => LspErrorCode::ServiceStopped,
        ProcessErrorCode::OutputLimit => LspErrorCode::OutputLimit,
        ProcessErrorCode::Unsupported => LspErrorCode::Unsupported,
        ProcessErrorCode::NotFound
        | ProcessErrorCode::PermissionDenied
        | ProcessErrorCode::Spawn
        | ProcessErrorCode::Sandbox => LspErrorCode::Spawn,
        _ => LspErrorCode::Io,
    };
    LspError::new(code)
}

/// Publish the local effect-owned LSP registry.
#[must_use]
pub fn lsp_registry_plugin() -> Box<dyn Plugin> {
    struct LspRegistryPlugin;

    impl Plugin for LspRegistryPlugin {
        fn name(&self) -> &'static str {
            "lsp-registry"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_LSP]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_FILESYSTEM, SERVICE_SUBPROCESS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let filesystem = context
                .get::<FileSystemService>(SERVICE_FILESYSTEM)
                .ok_or_else(|| CoreError::other("filesystem service type mismatch"))?;
            let subprocess = context
                .get::<SubprocessService>(SERVICE_SUBPROCESS)
                .ok_or_else(|| CoreError::other("subprocess service type mismatch"))?;
            let service = LspService::local((*subprocess).clone(), (*filesystem).clone());
            let disposer = service.clone();
            context.effect(move || disposer.close());
            context.provide(SERVICE_LSP, self.name(), service)
        }
    }

    Box::new(LspRegistryPlugin)
}

/// Register exact local stdio language servers into [`LspService`].
#[must_use]
pub fn lsp_stdio_plugin(definitions: Vec<LspServerDefinition>) -> Box<dyn Plugin> {
    struct LspStdioPlugin(Vec<LspServerDefinition>);

    impl Plugin for LspStdioPlugin {
        fn name(&self) -> &'static str {
            "lsp-stdio"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::ExternalProcess],
            )
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            let mut ids = self
                .0
                .iter()
                .map(|definition| definition.id.as_str().to_owned())
                .collect::<Vec<_>>();
            ids.sort();
            ids.into_iter()
                .map(|id| PluginContributionSpec::new(ContributionKind::ExternalProcess, id))
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_LSP]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let service = context
                .get::<LspService>(SERVICE_LSP)
                .ok_or_else(|| CoreError::other("LSP service type mismatch"))?;
            for definition in &self.0 {
                service
                    .register_effect(context, definition.clone())
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            Ok(())
        }
    }

    Box::new(LspStdioPlugin(definitions))
}
