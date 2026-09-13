//! Replaceable high-level filesystem service.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{
    EditFileOutput, EditFileSpec, FileMetadata, FileSystemError, FileSystemPolicy, GlobOutput,
    GlobSpec, GrepOutput, GrepOutputMode, GrepSpec, ObservationLog, PathRequest, ReadFileOutput,
    ReadFileSpec, ResolvedPath, WriteFileSpec,
};

/// Provider implementation behind [`FileSystemService`].
///
/// Implementations own path resolution and observation enforcement, not only
/// raw I/O. That keeps local, remote, and sandboxed providers behaviorally
/// interchangeable for model-facing Consumers.
#[async_trait]
pub trait FileSystemBackend: Send + Sync {
    /// Apply a revision-checked create/replace. Providers must opt in; the
    /// default never silently degrades a guard into an ordinary write.
    async fn write_checked(
        &self,
        _spec: super::CheckedWriteSpec,
        _cancellation: CancellationToken,
    ) -> Result<super::CheckedWriteOutput, FileSystemError> {
        Err(FileSystemError::new(
            super::FileSystemErrorCode::UnsupportedOperation,
        ))
    }

    /// Validate a complete single-file edit set before committing any changes.
    /// The default refuses without performing an individual edit.
    async fn edit_many(
        &self,
        _spec: super::MultiEditSpec,
        _cancellation: CancellationToken,
    ) -> Result<super::MultiEditOutput, FileSystemError> {
        Err(FileSystemError::new(
            super::FileSystemErrorCode::UnsupportedOperation,
        ))
    }
    /// Resolve caller intent to one exact provider path.
    ///
    /// # Errors
    /// Invalid paths or a stopped provider fail with a fixed
    /// [`FileSystemError`].
    fn resolve(&self, request: PathRequest) -> Result<ResolvedPath, FileSystemError>;

    /// Provider-owned observation registry.
    fn observations(&self) -> ObservationLog;

    /// Effective canonical root authority for this Provider generation.
    fn policy(&self) -> FileSystemPolicy;

    /// Read safe metadata for one resolved path.
    ///
    /// # Errors
    /// Missing, inaccessible, cancelled, or stopped operations fail safely.
    async fn metadata(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<FileMetadata, FileSystemError>;

    /// Create a directory tree.
    ///
    /// # Errors
    /// Invalid entry state, denial, cancellation, or I/O fails safely.
    async fn create_dir_all(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError>;

    /// Execute one bounded text read and record its successful observation.
    ///
    /// # Errors
    /// Missing/binary input, denial, cancellation, or I/O fails safely.
    async fn read(
        &self,
        spec: ReadFileSpec,
        cancellation: CancellationToken,
    ) -> Result<ReadFileOutput, FileSystemError>;

    /// Execute one full-file write, enforcing read-before-overwrite.
    ///
    /// # Errors
    /// Blind overwrite, denial, cancellation, or I/O fails safely.
    async fn write(
        &self,
        spec: WriteFileSpec,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError>;

    /// Execute one exact-string edit, enforcing a fresh prior read.
    ///
    /// # Errors
    /// Missing/stale observations, invalid match count, text decoding, denial,
    /// cancellation, or I/O fails safely.
    async fn edit(
        &self,
        spec: EditFileSpec,
        cancellation: CancellationToken,
    ) -> Result<EditFileOutput, FileSystemError>;

    /// Execute one bounded glob.
    ///
    /// # Errors
    /// Invalid roots/patterns, denial, cancellation, or I/O fails safely.
    async fn glob(
        &self,
        spec: GlobSpec,
        cancellation: CancellationToken,
    ) -> Result<GlobOutput, FileSystemError>;

    /// Execute one bounded regular-expression search.
    ///
    /// # Errors
    /// Invalid roots/patterns, denial, cancellation, or I/O fails safely.
    async fn grep(
        &self,
        spec: GrepSpec,
        cancellation: CancellationToken,
    ) -> Result<GrepOutput, FileSystemError>;
}

/// Typed wrapper around one replaceable filesystem provider.
#[derive(Clone)]
pub struct FileSystemService {
    backend: Arc<dyn FileSystemBackend>,
}

impl FileSystemService {
    /// Bind a provider implementation.
    #[must_use]
    pub fn new(backend: Arc<dyn FileSystemBackend>) -> Self {
        Self { backend }
    }

    /// Construct a standalone local service with explicit root authority.
    /// Shipped worlds use [`crate::local_filesystem_plugin`] so context
    /// shutdown owns the provider lifecycle.
    ///
    /// # Errors
    /// Every declared root must resolve to one unique accessible directory.
    pub fn local(policy: FileSystemPolicy) -> Result<Self, FileSystemError> {
        Ok(Self::new(Arc::new(
            super::local::LocalFileSystemBackend::new(policy)?,
        )))
    }

    /// Resolve caller intent to one exact provider path.
    ///
    /// # Errors
    /// Invalid paths or a stopped provider fail safely.
    pub fn resolve(&self, request: PathRequest) -> Result<ResolvedPath, FileSystemError> {
        self.backend.resolve(request)
    }

    /// Clone the provider-owned observation registry.
    #[must_use]
    pub fn observations(&self) -> ObservationLog {
        self.backend.observations()
    }

    /// Effective canonical root authority.
    #[must_use]
    pub fn policy(&self) -> FileSystemPolicy {
        self.backend.policy()
    }

    /// Read safe metadata for one resolved path.
    ///
    /// # Errors
    /// Missing, inaccessible, cancelled, or stopped operations fail safely.
    pub async fn metadata(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<FileMetadata, FileSystemError> {
        self.backend.metadata(path, cancellation).await
    }

    /// Create a directory tree.
    ///
    /// # Errors
    /// Invalid entry state, denial, cancellation, or I/O fails safely.
    pub async fn create_dir_all(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError> {
        self.backend.create_dir_all(path, cancellation).await
    }

    /// Execute one bounded text read and record its successful observation.
    ///
    /// # Errors
    /// Missing/binary input, denial, cancellation, or I/O fails safely.
    pub async fn read(
        &self,
        spec: ReadFileSpec,
        cancellation: CancellationToken,
    ) -> Result<ReadFileOutput, FileSystemError> {
        let max_bytes = spec.max_bytes();
        let window = spec.window().copied();
        let expected_revision = spec.expected_revision().map(str::to_owned);
        let output = self.backend.read(spec, cancellation).await?;
        if output.bytes().len() > max_bytes {
            return Err(FileSystemError::new(
                super::FileSystemErrorCode::InvalidOutput,
            ));
        }
        if let Some(window) = window {
            let page = output
                .page()
                .ok_or_else(|| FileSystemError::new(super::FileSystemErrorCode::InvalidOutput))?;
            let text = std::str::from_utf8(output.bytes())
                .map_err(|_| FileSystemError::new(super::FileSystemErrorCode::InvalidOutput))?;
            if page.start_line == 0
                || page.lines_returned > window.limit
                || page.lines_returned != text.lines().count()
                || page.start_byte.saturating_add(output.bytes().len() as u64) > page.total_bytes
                || page
                    .next_byte_offset
                    .is_some_and(|next| next <= page.start_byte || next >= page.total_bytes)
                || page.next_offset.is_some_and(|next| next <= page.start_line)
                || super::model::validate_revision(&page.revision).is_err()
                || expected_revision
                    .as_ref()
                    .is_some_and(|expected| expected != &page.revision)
            {
                return Err(FileSystemError::new(
                    super::FileSystemErrorCode::InvalidOutput,
                ));
            }
        }
        Ok(output)
    }

    /// Execute one full-file write, enforcing read-before-overwrite.
    ///
    /// # Errors
    /// Blind overwrite, denial, cancellation, or I/O fails safely.
    pub async fn write(
        &self,
        spec: WriteFileSpec,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError> {
        self.backend.write(spec, cancellation).await
    }

    /// Create a new file or explicitly replace one at a known revision.
    ///
    /// # Errors
    /// Unsupported providers, stale revisions, existing create targets or I/O.
    pub async fn write_checked(
        &self,
        spec: super::CheckedWriteSpec,
        cancellation: CancellationToken,
    ) -> Result<super::CheckedWriteOutput, FileSystemError> {
        let bytes = spec.write().bytes().len();
        let output = self.backend.write_checked(spec, cancellation).await?;
        if output.bytes != bytes || super::model::validate_revision(&output.revision).is_err() {
            return Err(FileSystemError::new(
                super::FileSystemErrorCode::InvalidOutput,
            ));
        }
        Ok(output)
    }

    /// Commit an entire ordered single-file edit set, or preview it unchanged.
    ///
    /// # Errors
    /// Any invalid/stale/ambiguous edit, unsupported provider, cancellation or I/O.
    pub async fn edit_many(
        &self,
        spec: super::MultiEditSpec,
        cancellation: CancellationToken,
    ) -> Result<super::MultiEditOutput, FileSystemError> {
        let count = spec.edits().len();
        let dry_run = spec.dry_run();
        let single = spec
            .edits()
            .iter()
            .map(|edit| !edit.replace_all())
            .collect::<Vec<_>>();
        let output = self.backend.edit_many(spec, cancellation).await?;
        if output.edits.len() != count
            || output.dry_run != dry_run
            || super::model::validate_revision(&output.revision).is_err()
            || output.edits.iter().zip(single).any(|(edit, single)| {
                edit.line() == 0
                    || edit.replacements() == 0
                    || (single && edit.replacements() != 1)
                    || !valid_edit_context(edit)
                    || edit.removed_line().contains('\0')
                    || edit.inserted_line().contains('\0')
                    || !checked_total_bytes([edit.removed_line().len(), edit.inserted_line().len()])
            })
        {
            return Err(FileSystemError::new(
                super::FileSystemErrorCode::InvalidOutput,
            ));
        }
        Ok(output)
    }

    /// Execute one exact-string edit, enforcing a fresh prior read.
    ///
    /// # Errors
    /// Missing/stale observations, invalid match count, text decoding, denial,
    /// cancellation, or I/O fails safely.
    pub async fn edit(
        &self,
        spec: EditFileSpec,
        cancellation: CancellationToken,
    ) -> Result<EditFileOutput, FileSystemError> {
        let replace_all = spec.replace_all();
        let output = self.backend.edit(spec, cancellation).await?;
        if output.line() == 0
            || output.replacements() == 0
            || (!replace_all && output.replacements() != 1)
            || !valid_edit_context(&output)
            || output.removed_line().contains('\0')
            || output.inserted_line().contains('\0')
            || !checked_total_bytes([output.removed_line().len(), output.inserted_line().len()])
        {
            return Err(FileSystemError::new(
                super::FileSystemErrorCode::InvalidOutput,
            ));
        }
        Ok(output)
    }

    /// Execute one bounded glob.
    ///
    /// # Errors
    /// Invalid roots/patterns, denial, cancellation, or I/O fails safely.
    pub async fn glob(
        &self,
        spec: GlobSpec,
        cancellation: CancellationToken,
    ) -> Result<GlobOutput, FileSystemError> {
        let result_limit = spec.result_limit();
        let output = self.backend.glob(spec, cancellation).await?;
        if !valid_glob_output(&output, result_limit) {
            return Err(FileSystemError::new(
                super::FileSystemErrorCode::InvalidOutput,
            ));
        }
        Ok(output)
    }

    /// Execute one bounded regular-expression search.
    ///
    /// # Errors
    /// Invalid roots/patterns, denial, cancellation, or I/O fails safely.
    pub async fn grep(
        &self,
        spec: GrepSpec,
        cancellation: CancellationToken,
    ) -> Result<GrepOutput, FileSystemError> {
        let result_limit = spec.result_limit();
        let output_mode = spec.output_mode();
        let offset = spec.offset();
        let before_context = spec.before_context();
        let after_context = spec.after_context();
        let output = self.backend.grep(spec, cancellation).await?;
        if !valid_grep_output(
            &output,
            output_mode,
            offset,
            result_limit,
            before_context,
            after_context,
        ) {
            return Err(FileSystemError::new(
                super::FileSystemErrorCode::InvalidOutput,
            ));
        }
        Ok(output)
    }
}

fn valid_edit_context(output: &EditFileOutput) -> bool {
    output.context().is_none_or(|context| {
        context.before.len() <= 3
            && context.before.len() < output.line()
            && context.after.len() <= 3
            && context.removed_lines <= MAX_PROVIDER_RESULT_BYTES
            && context.inserted_lines <= MAX_PROVIDER_RESULT_BYTES
            && context
                .before
                .iter()
                .chain(&context.after)
                .all(|line| line.len() <= 8192 && !line.contains(['\n', '\0']))
    })
}

const MAX_PROVIDER_RESULT_BYTES: usize = 64 * 1024 * 1024;

fn valid_glob_output(output: &GlobOutput, limit: usize) -> bool {
    output.matches().len() <= limit
        && output.matches().len() <= output.total_matches()
        && output.matches().windows(2).all(|pair| pair[0] < pair[1])
        && output
            .matches()
            .iter()
            .all(|path| valid_relative_output_path(path))
        && checked_total_bytes(output.matches().iter().map(String::len))
}

fn valid_grep_output(
    output: &GrepOutput,
    mode: GrepOutputMode,
    offset: usize,
    limit: usize,
    before_context: usize,
    after_context: usize,
) -> bool {
    let ordered = output
        .matches()
        .windows(2)
        .all(|pair| (pair[0].path(), pair[0].line()) < (pair[1].path(), pair[1].line()));
    let valid_content = output.matches().len() <= limit
        && output.matches().len() <= output.total_matches()
        && (output.matches().is_empty()
            || offset.saturating_add(output.matches().len()) <= output.total_matches())
        && ordered
        && output.matches().iter().all(|entry| {
            entry.line() > 0
                && valid_relative_output_path(entry.path())
                && !entry
                    .text()
                    .chars()
                    .any(|character| character.is_control() && character != '\t')
                && entry.before().len() <= before_context
                && entry.after().len() <= after_context
                && entry
                    .before()
                    .windows(2)
                    .all(|pair| pair[0].line() < pair[1].line())
                && entry
                    .after()
                    .windows(2)
                    .all(|pair| pair[0].line() < pair[1].line())
                && entry.before().iter().all(|line| {
                    line.line() > 0
                        && line.line() < entry.line()
                        && !line
                            .text()
                            .chars()
                            .any(|character| character.is_control() && character != '\t')
                })
                && entry.after().iter().all(|line| {
                    line.line() > entry.line()
                        && !line
                            .text()
                            .chars()
                            .any(|character| character.is_control() && character != '\t')
                })
        })
        && checked_total_bytes(output.matches().iter().flat_map(|entry| {
            std::iter::once(entry.path().len())
                .chain(std::iter::once(entry.text().len()))
                .chain(entry.before().iter().map(|line| line.text().len()))
                .chain(entry.after().iter().map(|line| line.text().len()))
        }));
    let valid_files = output.files().len() <= limit
        && output.files().len() <= output.total_files()
        && output.total_files() <= output.total_matches()
        && (output.files().is_empty()
            || offset.saturating_add(output.files().len()) <= output.total_files())
        && output
            .files()
            .windows(2)
            .all(|pair| pair[0].path() < pair[1].path())
        && output
            .files()
            .iter()
            .all(|entry| entry.count() > 0 && valid_relative_output_path(entry.path()))
        && output
            .files()
            .iter()
            .try_fold(0_usize, |sum, entry| sum.checked_add(entry.count()))
            .is_some_and(|retained| retained <= output.total_matches())
        && checked_total_bytes(output.files().iter().map(|entry| entry.path().len()));
    match mode {
        GrepOutputMode::Content => {
            output.files().is_empty() && output.total_files() == 0 && valid_content
        }
        GrepOutputMode::FilesWithMatches | GrepOutputMode::Count => {
            output.matches().is_empty() && valid_files
        }
    }
}

fn valid_relative_output_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.contains(':')
        })
}

fn checked_total_bytes(sizes: impl IntoIterator<Item = usize>) -> bool {
    sizes
        .into_iter()
        .try_fold(0_usize, |total, size| total.checked_add(size))
        .is_some_and(|total| total <= MAX_PROVIDER_RESULT_BYTES)
}

impl std::fmt::Debug for FileSystemService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileSystemService")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{valid_grep_output, valid_relative_output_path};
    use crate::{GrepContextLine, GrepFileMatch, GrepMatch, GrepOutput, GrepOutputMode};

    #[test]
    fn provider_paths_are_normalized_relative_and_portable() {
        assert!(valid_relative_output_path("src/lib.rs"));
        for invalid in [
            "../secret",
            "./src/lib.rs",
            "src//lib.rs",
            "/absolute",
            "C:/absolute",
            "src\\lib.rs",
            "src/\u{1b}[31m.rs",
        ] {
            assert!(!valid_relative_output_path(invalid), "accepted {invalid:?}");
        }
    }

    #[test]
    fn grep_provider_output_is_mode_specific_and_context_bounded() {
        let content_with_file_summary = GrepOutput::new(
            vec![GrepMatch::new("src/lib.rs".into(), 2, "needle".into())],
            1,
        )
        .with_files(vec![GrepFileMatch::new("src/lib.rs".into(), 1)], 1);
        assert!(!valid_grep_output(
            &content_with_file_summary,
            GrepOutputMode::Content,
            0,
            10,
            0,
            0
        ));

        let reversed_context = GrepOutput::new(
            vec![
                GrepMatch::new("src/lib.rs".into(), 3, "needle".into()).with_context(
                    vec![
                        GrepContextLine::new(2, "second".into()),
                        GrepContextLine::new(1, "first".into()),
                    ],
                    Vec::new(),
                ),
            ],
            1,
        );
        assert!(!valid_grep_output(
            &reversed_context,
            GrepOutputMode::Content,
            0,
            10,
            2,
            0
        ));

        let impossible_count = GrepOutput::new(Vec::new(), 1)
            .with_files(vec![GrepFileMatch::new("src/lib.rs".into(), 2)], 1);
        assert!(!valid_grep_output(
            &impossible_count,
            GrepOutputMode::Count,
            0,
            10,
            0,
            0
        ));
    }
}
