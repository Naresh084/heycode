//! Validated filesystem requests and bounded outcomes.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::{FileSystemError, FileSystemErrorCode};

const MAX_READ_BYTES: usize = 64 * 1024 * 1024;
const MAX_PATTERN_BYTES: usize = 64 * 1024;
const MAX_SEARCH_RESULTS: usize = 100_000;
const MAX_IGNORED_DIRECTORIES: usize = 256;
const MAX_GREP_CONTEXT_LINES: usize = 20;

/// Unresolved caller path and the absolute working directory it is relative to.
#[derive(Clone)]
pub struct PathRequest {
    cwd: PathBuf,
    path: PathBuf,
}

impl PathRequest {
    /// Validate one path-resolution request.
    ///
    /// # Errors
    /// The working directory must be absolute when `path` is relative; both
    /// values must be nonempty and NUL-free.
    pub fn new(cwd: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Result<Self, FileSystemError> {
        let cwd = cwd.into();
        let path = path.into();
        if (!path.is_absolute() && !cwd.is_absolute())
            || cwd.as_os_str().is_empty()
            || path.as_os_str().is_empty()
            || contains_nul(cwd.as_os_str())
            || contains_nul(path.as_os_str())
        {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self { cwd, path })
    }

    /// Working directory used only when [`Self::path`] is relative.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Caller-supplied absolute or relative path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Debug for PathRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PathRequest")
            .field("cwd", &"<redacted>")
            .field("path", &"<redacted>")
            .finish()
    }
}

/// One provider-resolved absolute filesystem path.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResolvedPath(PathBuf);

impl ResolvedPath {
    /// Construct a provider-resolved path.
    ///
    /// # Errors
    /// The value must be absolute and NUL-free.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, FileSystemError> {
        let path = path.into();
        if !path.is_absolute() || contains_nul(path.as_os_str()) {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self(path))
    }

    /// Exact absolute path used by the active provider.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl std::fmt::Debug for ResolvedPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResolvedPath(<redacted>)")
    }
}

/// Provider-observed entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileEntryKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Another host entry such as a device or socket.
    Other,
}

/// Safe metadata for one resolved path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMetadata {
    kind: FileEntryKind,
    len: u64,
}

impl FileMetadata {
    /// Construct provider metadata.
    #[must_use]
    pub const fn new(kind: FileEntryKind, len: u64) -> Self {
        Self { kind, len }
    }

    /// Entry kind.
    #[must_use]
    pub const fn kind(&self) -> FileEntryKind {
        self.kind
    }

    /// Entry size reported by the provider.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Whether the reported entry size is zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Explicit bounded read specification.
#[derive(Clone)]
pub struct ReadFileSpec {
    path: ResolvedPath,
    max_bytes: usize,
    binary: bool,
    window: Option<ReadFileWindow>,
    expected_revision: Option<String>,
}

impl ReadFileSpec {
    /// Build a read with an explicit byte ceiling.
    ///
    /// # Errors
    /// The ceiling must be between one byte and 64 MiB.
    pub fn new(path: ResolvedPath, max_bytes: usize) -> Result<Self, FileSystemError> {
        if !(1..=MAX_READ_BYTES).contains(&max_bytes) {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self {
            path,
            max_bytes,
            binary: false,
            window: None,
            expected_revision: None,
        })
    }

    /// Read exact bytes for a media Consumer without relaxing path, size, race or observation checks.
    /// Ordinary text reads retain their binary-content refusal.
    ///
    /// # Errors
    /// Same byte ceiling as [`Self::new`].
    pub fn new_binary(path: ResolvedPath, max_bytes: usize) -> Result<Self, FileSystemError> {
        let mut spec = Self::new(path, max_bytes)?;
        spec.binary = true;
        Ok(spec)
    }

    /// Whether the Consumer explicitly requested byte-exact media rather than text.
    #[must_use]
    pub const fn allows_binary(&self) -> bool {
        self.binary
    }

    /// Exact resolved target.
    #[must_use]
    pub fn path(&self) -> &ResolvedPath {
        &self.path
    }

    /// Explicit retained byte ceiling.
    #[must_use]
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Select a bounded text page. Offset is one-based; byte_offset is an
    /// optional exact continuation for a byte-capped long line.
    ///
    /// # Errors
    /// Binary reads, zero offsets/limits, or more than 2,000 requested lines.
    pub fn with_window(
        mut self,
        offset: usize,
        limit: usize,
        byte_offset: Option<u64>,
    ) -> Result<Self, FileSystemError> {
        if self.binary
            || offset == 0
            || !(1..=2_000).contains(&limit)
            || (byte_offset.is_some() && offset != 1)
        {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        self.window = Some(ReadFileWindow {
            offset,
            limit,
            byte_offset,
        });
        Ok(self)
    }

    /// Refuse pagination if the file revision changed since the previous page.
    ///
    /// # Errors
    /// Revision must be the 64-character lowercase hexadecimal token from read.
    pub fn with_expected_revision(mut self, revision: String) -> Result<Self, FileSystemError> {
        validate_revision(&revision)?;
        self.expected_revision = Some(revision);
        Ok(self)
    }

    /// Optional text page selection.
    #[must_use]
    pub fn window(&self) -> Option<&ReadFileWindow> {
        self.window.as_ref()
    }

    /// Optional revision precondition.
    #[must_use]
    pub fn expected_revision(&self) -> Option<&str> {
        self.expected_revision.as_deref()
    }
}

/// A bounded line page; constructed through ReadFileSpec.
#[derive(Debug, Clone, Copy)]
pub struct ReadFileWindow {
    /// One-based first requested line.
    pub offset: usize,
    /// Maximum requested lines.
    pub limit: usize,
    /// Absolute byte continuation for a previously truncated long line.
    pub byte_offset: Option<u64>,
}

/// Page metadata. Totals are exact only if the bounded scan reached EOF.
#[derive(Debug, Clone)]
pub struct ReadFilePage {
    /// Exact file size from the stable opened file.
    pub total_bytes: u64,
    /// Exact line count, or None if the scan budget ended before EOF.
    pub total_lines: Option<usize>,
    /// One-based line containing the first retained byte.
    pub start_line: usize,
    /// Absolute position of the first retained byte.
    pub start_byte: u64,
    /// Number of complete or partial lines in retained text.
    pub lines_returned: usize,
    /// Next exact byte position when more content remains, including long lines.
    pub next_byte_offset: Option<u64>,
    /// Next one-based line when continuation starts at a line boundary.
    pub next_offset: Option<usize>,
    /// Whether the retained last line is incomplete.
    pub partial_last_line: bool,
    /// True when the bounded scan did not reach EOF.
    pub scan_limited: bool,
    /// Opaque metadata revision for optimistic concurrency checks.
    pub revision: String,
}

pub(crate) fn validate_revision(revision: &str) -> Result<(), FileSystemError> {
    if revision.len() != 64
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
    }
    Ok(())
}

impl std::fmt::Debug for ReadFileSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadFileSpec")
            .field("path", &"<redacted>")
            .field("max_bytes", &self.max_bytes)
            .field("binary", &self.binary)
            .finish()
    }
}

/// Bounded text-read bytes.
pub struct ReadFileOutput {
    bytes: Vec<u8>,
    truncated: bool,
    page: Option<ReadFilePage>,
}

impl ReadFileOutput {
    /// Construct one provider read outcome. [`FileSystemService`](crate::FileSystemService)
    /// validates the retained size against the originating spec.
    #[must_use]
    pub fn new(bytes: Vec<u8>, truncated: bool) -> Self {
        Self {
            bytes,
            truncated,
            page: None,
        }
    }

    /// Attach page facts, validated by the service before publication.
    #[must_use]
    pub fn with_page(mut self, page: ReadFilePage) -> Self {
        self.page = Some(page);
        self
    }

    /// Page metadata, present only for a windowed text read.
    #[must_use]
    pub fn page(&self) -> Option<&ReadFilePage> {
        self.page.as_ref()
    }

    /// Retained bytes, never larger than the requested ceiling.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether bytes beyond the retained prefix existed.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

impl std::fmt::Debug for ReadFileOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadFileOutput")
            .field("retained_bytes", &self.bytes.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// Exact full-file replacement specification.
#[derive(Clone)]
pub struct WriteFileSpec {
    path: ResolvedPath,
    bytes: Vec<u8>,
}

impl WriteFileSpec {
    /// Build one full-file write. Existing targets still require a prior read
    /// when the service executes the spec.
    ///
    /// # Errors
    /// Payloads larger than 64 MiB are rejected at this initial local seam.
    pub fn new(path: ResolvedPath, bytes: impl AsRef<[u8]>) -> Result<Self, FileSystemError> {
        let bytes = bytes.as_ref();
        if bytes.len() > MAX_READ_BYTES {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self {
            path,
            bytes: bytes.to_vec(),
        })
    }

    /// Exact resolved target.
    #[must_use]
    pub fn path(&self) -> &ResolvedPath {
        &self.path
    }

    /// Exact replacement bytes. Call only at the provider operation that
    /// writes them; ordinary diagnostics remain redacted.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for WriteFileSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WriteFileSpec")
            .field("path", &"<redacted>")
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

/// Exact-string edit specification.
#[derive(Clone)]
pub struct EditFileSpec {
    path: ResolvedPath,
    old: String,
    new: String,
    replace_all: bool,
}

impl EditFileSpec {
    /// Build an edit whose old text is nonempty and whose total payload is
    /// bounded.
    ///
    /// # Errors
    /// Empty old text or a combined old/new payload over 64 MiB is invalid.
    pub fn new(
        path: ResolvedPath,
        old: impl Into<String>,
        new: impl Into<String>,
        replace_all: bool,
    ) -> Result<Self, FileSystemError> {
        let old = old.into();
        let new = new.into();
        if old.is_empty()
            || old
                .len()
                .checked_add(new.len())
                .is_none_or(|size| size > MAX_READ_BYTES)
        {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self {
            path,
            old,
            new,
            replace_all,
        })
    }

    /// Exact resolved target.
    #[must_use]
    pub fn path(&self) -> &ResolvedPath {
        &self.path
    }

    /// Exact text to replace. Expose only at provider execution.
    #[must_use]
    pub fn old(&self) -> &str {
        &self.old
    }

    /// Exact replacement text. Expose only at provider execution.
    #[must_use]
    pub fn new_text(&self) -> &str {
        &self.new
    }

    /// Whether every occurrence may be replaced.
    #[must_use]
    pub const fn replace_all(&self) -> bool {
        self.replace_all
    }
}

impl std::fmt::Debug for EditFileSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EditFileSpec")
            .field("path", &"<redacted>")
            .field("old_bytes", &self.old.len())
            .field("new_bytes", &self.new.len())
            .field("replace_all", &self.replace_all)
            .finish()
    }
}

/// Bounded unchanged context around the first replacement. Counts refer to
/// complete affected source lines, before any display text truncation.
#[derive(Debug)]
pub struct EditContext {
    /// Up to three unchanged lines immediately preceding the replacement.
    pub before: Vec<String>,
    /// Up to three unchanged lines immediately following the replacement.
    pub after: Vec<String>,
    /// Number of affected original lines in the first replacement span.
    pub removed_lines: usize,
    /// Number of affected replacement lines in the first replacement span.
    pub inserted_lines: usize,
}

/// Safe mutation facts plus the first changed line for model-facing diff
/// rendering.
pub struct EditFileOutput {
    replacements: usize,
    line: usize,
    removed_line: String,
    inserted_line: String,
    diff_truncated: bool,
    context: Option<EditContext>,
}

impl EditFileOutput {
    /// Construct provider edit facts. The service validates them against the
    /// originating spec before publication.
    #[must_use]
    pub fn new(
        replacements: usize,
        line: usize,
        removed_line: String,
        inserted_line: String,
    ) -> Self {
        Self {
            replacements,
            line,
            removed_line,
            inserted_line,
            diff_truncated: false,
            context: None,
        }
    }

    /// Attach bounded unchanged context around the first replacement.
    #[must_use]
    pub fn with_context(mut self, context: EditContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Provider-supplied unchanged source context, when available.
    #[must_use]
    pub fn context(&self) -> Option<&EditContext> {
        self.context.as_ref()
    }

    /// Number of committed replacements.
    #[must_use]
    pub const fn replacements(&self) -> usize {
        self.replacements
    }

    /// One-based line number of the first replacement.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// Complete original line containing the first replacement.
    #[must_use]
    pub fn removed_line(&self) -> &str {
        &self.removed_line
    }

    /// Complete replacement line for the first replacement.
    #[must_use]
    pub fn inserted_line(&self) -> &str {
        &self.inserted_line
    }

    /// Whether bounded display text omits part of the changed line.
    #[must_use]
    pub fn diff_truncated(&self) -> bool {
        self.diff_truncated
    }

    /// Mark provider-side preview truncation without changing replacement facts.
    #[must_use]
    pub fn with_diff_truncated(mut self, truncated: bool) -> Self {
        self.diff_truncated = truncated;
        self
    }
}

impl std::fmt::Debug for EditFileOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EditFileOutput")
            .field("replacements", &self.replacements)
            .field("line", &self.line)
            .field("removed_bytes", &self.removed_line.len())
            .field("inserted_bytes", &self.inserted_line.len())
            .finish()
    }
}

/// Bounded provider-side glob specification.
#[derive(Clone)]
pub struct GlobSpec {
    root: ResolvedPath,
    pattern: String,
    ignored_directories: Vec<OsString>,
    result_limit: usize,
}

impl GlobSpec {
    /// Build a glob with explicit ignored-directory names and result ceiling.
    ///
    /// # Errors
    /// Oversized patterns, invalid ignored names, or a zero/excessive limit
    /// fail before filesystem access.
    pub fn new<I, S>(
        root: ResolvedPath,
        pattern: impl Into<String>,
        ignored_directories: I,
        result_limit: usize,
    ) -> Result<Self, FileSystemError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let pattern = pattern.into();
        let ignored_directories = validate_search(
            &pattern,
            ignored_directories.into_iter().map(Into::into).collect(),
            result_limit,
        )?;
        Ok(Self {
            root,
            pattern,
            ignored_directories,
            result_limit,
        })
    }

    /// Exact resolved search root.
    #[must_use]
    pub fn root(&self) -> &ResolvedPath {
        &self.root
    }

    /// Exact glob expression. Expose only at provider execution.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Explicit directory basenames to skip.
    #[must_use]
    pub fn ignored_directories(&self) -> &[OsString] {
        &self.ignored_directories
    }

    /// Explicit retained-result ceiling.
    #[must_use]
    pub const fn result_limit(&self) -> usize {
        self.result_limit
    }
}

impl std::fmt::Debug for GlobSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GlobSpec")
            .field("root", &"<redacted>")
            .field("pattern", &"<redacted>")
            .field("ignored_directory_count", &self.ignored_directories.len())
            .field("result_limit", &self.result_limit)
            .finish()
    }
}

/// Coverage limitations for a bounded search. Counts describe observed omissions;
/// they are not estimates of matches in content that was not searched.
#[derive(Clone, Debug, Default)]
pub struct SearchReport {
    /// Entries or files that could not be read.
    pub unreadable: usize,
    /// Paths or lines that cannot be represented as UTF-8.
    pub non_utf8: usize,
    /// Paths that cannot be represented by the portable search result contract.
    pub unsupported_paths: usize,
    /// Lines containing non-text control characters.
    pub non_text_lines: usize,
    /// Lines skipped because they exceed the per-line scan limit.
    pub oversized_lines: usize,
    /// Files stopped at the per-file byte limit.
    pub limited_files: usize,
    /// Traversal or aggregate scanning reached its resource budget.
    pub budget_exhausted: bool,
    /// Matching excerpts shortened to fit the output budget.
    pub shortened_excerpts: usize,
    /// Matching rows omitted because their output would exceed the byte budget.
    pub omitted_rows: usize,
}

impl SearchReport {
    /// Whether the match count is only a lower bound on searchable content.
    #[must_use]
    pub const fn incomplete(&self) -> bool {
        self.unreadable > 0
            || self.non_utf8 > 0
            || self.unsupported_paths > 0
            || self.non_text_lines > 0
            || self.oversized_lines > 0
            || self.limited_files > 0
            || self.budget_exhausted
    }

    /// Human-readable coverage notice suitable for tool output.
    #[must_use]
    pub fn notice(&self) -> Option<String> {
        let mut reasons = Vec::new();
        if self.unreadable > 0 {
            reasons.push(format!("{} unreadable entries/files", self.unreadable));
        }
        if self.non_utf8 > 0 {
            reasons.push(format!("{} non-UTF-8 paths/lines skipped", self.non_utf8));
        }
        if self.unsupported_paths > 0 {
            reasons.push(format!(
                "{} paths not representable in search output",
                self.unsupported_paths
            ));
        }
        if self.non_text_lines > 0 {
            reasons.push(format!("{} non-text lines skipped", self.non_text_lines));
        }
        if self.oversized_lines > 0 {
            reasons.push(format!("{} oversized lines skipped", self.oversized_lines));
        }
        if self.limited_files > 0 {
            reasons.push(format!(
                "{} files reached the scan limit",
                self.limited_files
            ));
        }
        if self.budget_exhausted {
            reasons.push("search budget exhausted".to_owned());
        }
        if self.shortened_excerpts > 0 {
            reasons.push(format!("{} excerpts shortened", self.shortened_excerpts));
        }
        if self.omitted_rows > 0 {
            reasons.push(format!(
                "{} rows omitted by output byte limit",
                self.omitted_rows
            ));
        }
        if reasons.is_empty() {
            return None;
        }
        let prefix = if self.incomplete() {
            "Partial search; match count is a lower bound"
        } else {
            "Output limited"
        };
        Some(format!(
            "({prefix}: {}. Narrow the path/include or read the matching file.)",
            reasons.join(", ")
        ))
    }
}

/// Bounded glob outcome.
pub struct GlobOutput {
    matches: Vec<String>,
    total_matches: usize,
    report: SearchReport,
}

impl GlobOutput {
    /// Construct one bounded provider glob outcome. The service validates the
    /// retained count and ordering before publication.
    #[must_use]
    pub fn new(matches: Vec<String>, total_matches: usize) -> Self {
        Self {
            matches,
            total_matches,
            report: SearchReport::default(),
        }
    }

    /// Attach coverage limitations from the provider.
    #[must_use]
    pub fn with_report(mut self, report: SearchReport) -> Self {
        self.report = report;
        self
    }

    /// Coverage and output limitations for this search.
    #[must_use]
    pub fn report(&self) -> &SearchReport {
        &self.report
    }

    /// Retained normalized paths relative to the search root.
    #[must_use]
    pub fn matches(&self) -> &[String] {
        &self.matches
    }

    /// Observed matches before the result ceiling. A lower bound if the report is incomplete.
    #[must_use]
    pub const fn total_matches(&self) -> usize {
        self.total_matches
    }
}

impl std::fmt::Debug for GlobOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GlobOutput")
            .field("retained_matches", &self.matches.len())
            .field("total_matches", &self.total_matches)
            .finish()
    }
}

/// Bounded provider-side regular-expression search specification.
#[derive(Clone)]
pub struct GrepSpec {
    root: ResolvedPath,
    pattern: String,
    include: Option<String>,
    ignored_directories: Vec<OsString>,
    result_limit: usize,
    output_mode: GrepOutputMode,
    offset: usize,
    before_context: usize,
    after_context: usize,
    case_insensitive: bool,
}

/// Granularity retained by a grep provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GrepOutputMode {
    /// Matching source lines, optionally with bounded neighboring lines.
    #[default]
    Content,
    /// One entry for each file containing at least one matching line.
    FilesWithMatches,
    /// One entry per matching file with its matching-line count.
    Count,
}

impl GrepSpec {
    /// Build a grep with explicit filename filter, ignored directories, and
    /// result ceiling.
    ///
    /// # Errors
    /// Oversized patterns/filters, invalid ignored names, or invalid limits
    /// fail before filesystem access. Regex syntax is checked by the provider.
    pub fn new<I, S>(
        root: ResolvedPath,
        pattern: impl Into<String>,
        include: Option<&str>,
        ignored_directories: I,
        result_limit: usize,
    ) -> Result<Self, FileSystemError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let pattern = pattern.into();
        let ignored_directories = validate_search(
            &pattern,
            ignored_directories.into_iter().map(Into::into).collect(),
            result_limit,
        )?;
        let include = include.map(str::to_owned);
        if include
            .as_ref()
            .is_some_and(|value| value.len() > MAX_PATTERN_BYTES)
        {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        Ok(Self {
            root,
            pattern,
            include,
            ignored_directories,
            result_limit,
            output_mode: GrepOutputMode::Content,
            offset: 0,
            before_context: 0,
            after_context: 0,
            case_insensitive: false,
        })
    }

    /// Select bounded output and matching behavior.
    ///
    /// # Errors
    /// Offsets above 100,000 or more than 20 context lines are refused before
    /// provider access. Context is meaningful only for content output.
    pub fn with_options(
        mut self,
        output_mode: GrepOutputMode,
        offset: usize,
        before_context: usize,
        after_context: usize,
        case_insensitive: bool,
    ) -> Result<Self, FileSystemError> {
        if offset > MAX_SEARCH_RESULTS
            || before_context > MAX_GREP_CONTEXT_LINES
            || after_context > MAX_GREP_CONTEXT_LINES
            || (output_mode != GrepOutputMode::Content
                && (before_context != 0 || after_context != 0))
        {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        self.output_mode = output_mode;
        self.offset = offset;
        self.before_context = before_context;
        self.after_context = after_context;
        self.case_insensitive = case_insensitive;
        Ok(self)
    }

    /// Exact resolved search root.
    #[must_use]
    pub fn root(&self) -> &ResolvedPath {
        &self.root
    }

    /// Exact regular expression. Expose only at provider execution.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Optional filename glob.
    #[must_use]
    pub fn include(&self) -> Option<&str> {
        self.include.as_deref()
    }

    /// Explicit directory basenames to skip.
    #[must_use]
    pub fn ignored_directories(&self) -> &[OsString] {
        &self.ignored_directories
    }

    /// Explicit retained-result ceiling.
    #[must_use]
    pub const fn result_limit(&self) -> usize {
        self.result_limit
    }

    /// Requested result granularity.
    #[must_use]
    pub const fn output_mode(&self) -> GrepOutputMode {
        self.output_mode
    }

    /// Number of matching entries to skip before retaining output.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Maximum preceding lines retained for each content match.
    #[must_use]
    pub const fn before_context(&self) -> usize {
        self.before_context
    }

    /// Maximum following lines retained for each content match.
    #[must_use]
    pub const fn after_context(&self) -> usize {
        self.after_context
    }

    /// Whether the provider should match without case distinctions.
    #[must_use]
    pub const fn case_insensitive(&self) -> bool {
        self.case_insensitive
    }
}

impl std::fmt::Debug for GrepSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrepSpec")
            .field("root", &"<redacted>")
            .field("pattern", &"<redacted>")
            .field("has_include", &self.include.is_some())
            .field("ignored_directory_count", &self.ignored_directories.len())
            .field("result_limit", &self.result_limit)
            .field("output_mode", &self.output_mode)
            .field("offset", &self.offset)
            .field("before_context", &self.before_context)
            .field("after_context", &self.after_context)
            .field("case_insensitive", &self.case_insensitive)
            .finish()
    }
}

/// One bounded non-matching line neighboring a retained grep match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepContextLine {
    line: usize,
    text: String,
}

impl GrepContextLine {
    /// Construct one provider context line.
    #[must_use]
    pub fn new(line: usize, text: String) -> Self {
        Self { line, text }
    }

    /// One-based source line number.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// Bounded source text without its line ending.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// One retained grep match.
pub struct GrepMatch {
    path: String,
    line: usize,
    text: String,
    before: Vec<GrepContextLine>,
    after: Vec<GrepContextLine>,
}

impl GrepMatch {
    /// Construct one provider match. The service validates the path/line and
    /// ordering before publication.
    #[must_use]
    pub fn new(path: String, line: usize, text: String) -> Self {
        Self {
            path,
            line,
            text,
            before: Vec::new(),
            after: Vec::new(),
        }
    }

    /// Attach bounded neighboring source lines.
    #[must_use]
    pub fn with_context(
        mut self,
        before: Vec<GrepContextLine>,
        after: Vec<GrepContextLine>,
    ) -> Self {
        self.before = before;
        self.after = after;
        self
    }

    /// Normalized path relative to the search base.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// One-based source line number.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// Bounded matching source line without its line ending.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Retained preceding source lines in ascending order.
    #[must_use]
    pub fn before(&self) -> &[GrepContextLine] {
        &self.before
    }

    /// Retained following source lines in ascending order.
    #[must_use]
    pub fn after(&self) -> &[GrepContextLine] {
        &self.after
    }
}

impl std::fmt::Debug for GrepMatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrepMatch")
            .field("path", &"<redacted>")
            .field("line", &self.line)
            .field("text_bytes", &self.text.len())
            .field("before_lines", &self.before.len())
            .field("after_lines", &self.after.len())
            .finish()
    }
}

/// One matching file and its observed matching-line count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepFileMatch {
    path: String,
    count: usize,
}

impl GrepFileMatch {
    /// Construct one provider file summary.
    #[must_use]
    pub fn new(path: String, count: usize) -> Self {
        Self { path, count }
    }

    /// Normalized path relative to the search base.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Matching source lines observed in this file.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }
}

/// Bounded grep outcome.
pub struct GrepOutput {
    matches: Vec<GrepMatch>,
    total_matches: usize,
    files: Vec<GrepFileMatch>,
    total_files: usize,
    report: SearchReport,
}

impl GrepOutput {
    /// Construct one bounded provider grep outcome. The service validates the
    /// retained count and ordering before publication.
    #[must_use]
    pub fn new(matches: Vec<GrepMatch>, total_matches: usize) -> Self {
        Self {
            matches,
            total_matches,
            files: Vec::new(),
            total_files: 0,
            report: SearchReport::default(),
        }
    }

    /// Attach bounded per-file summaries for file and count modes.
    #[must_use]
    pub fn with_files(mut self, files: Vec<GrepFileMatch>, total_files: usize) -> Self {
        self.files = files;
        self.total_files = total_files;
        self
    }

    /// Attach coverage limitations from the provider.
    #[must_use]
    pub fn with_report(mut self, report: SearchReport) -> Self {
        self.report = report;
        self
    }

    /// Coverage and output limitations for this search.
    #[must_use]
    pub fn report(&self) -> &SearchReport {
        &self.report
    }

    /// Retained matches in stable provider order.
    #[must_use]
    pub fn matches(&self) -> &[GrepMatch] {
        &self.matches
    }

    /// Observed matches before the result ceiling. A lower bound if the report is incomplete.
    #[must_use]
    pub const fn total_matches(&self) -> usize {
        self.total_matches
    }

    /// Retained matching-file summaries.
    #[must_use]
    pub fn files(&self) -> &[GrepFileMatch] {
        &self.files
    }

    /// Observed matching files before offset and result ceilings.
    #[must_use]
    pub const fn total_files(&self) -> usize {
        self.total_files
    }
}

impl std::fmt::Debug for GrepOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrepOutput")
            .field("retained_matches", &self.matches.len())
            .field("total_matches", &self.total_matches)
            .field("retained_files", &self.files.len())
            .field("total_files", &self.total_files)
            .finish()
    }
}

fn validate_search(
    pattern: &str,
    ignored_directories: Vec<OsString>,
    result_limit: usize,
) -> Result<Vec<OsString>, FileSystemError> {
    let invalid_name = ignored_directories.iter().any(|name| {
        let path = Path::new(name);
        path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
            || contains_nul(name)
    });
    if pattern.len() > MAX_PATTERN_BYTES
        || !(1..=MAX_SEARCH_RESULTS).contains(&result_limit)
        || ignored_directories.len() > MAX_IGNORED_DIRECTORIES
        || invalid_name
    {
        return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
    }
    Ok(ignored_directories)
}

fn contains_nul(value: &std::ffi::OsStr) -> bool {
    value.as_encoded_bytes().contains(&0)
}
