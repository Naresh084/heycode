//! Capability-directory-backed local filesystem Provider.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead as _, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cap_std::fs::{Dir, OpenOptions, Permissions};
use regex::RegexBuilder;
use tokio_util::sync::CancellationToken;

use super::observation::Stamp;

type CommitHook = Arc<dyn Fn(&Path) + Send + Sync>;
use super::{
    EditFileOutput, EditFileSpec, FileEntryKind, FileMetadata, FileSystemBackend, FileSystemError,
    FileSystemErrorCode, FileSystemPolicy, FileSystemRoot, FileSystemRootAccess, GlobOutput,
    GlobSpec, GrepContextLine, GrepFileMatch, GrepMatch, GrepOutput, GrepOutputMode, GrepSpec,
    ObservationLog, PathRequest, ReadFileOutput, ReadFileSpec, ResolvedPath, WriteFileSpec,
    remove_current_components,
};

const MAX_SEARCH_ENTRIES: usize = 100_000;
const MAX_SEARCH_PATH_BYTES: usize = 16 * 1024 * 1024;
const MAX_SEARCH_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEARCH_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SEARCH_LINE_BYTES: usize = 1024 * 1024;
const MAX_SEARCH_EXCERPT_BYTES: usize = 4096;
const MAX_SEARCH_OUTPUT_BYTES: usize = 24 * 1024;

const MAX_EDIT_BYTES: usize = 64 * 1024 * 1024;
const MAX_EDIT_DIFF_BYTES: usize = 8192;

struct PendingGrepMatch {
    path: String,
    line: usize,
    text: String,
    before: Vec<GrepContextLine>,
    after: Vec<GrepContextLine>,
    after_limit: usize,
    after_seen: usize,
}

fn bounded_search_excerpt(text: &str) -> (String, bool) {
    let mut end = text.len().min(MAX_SEARCH_EXCERPT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut excerpt = text[..end].to_owned();
    let shortened = end < text.len();
    if shortened {
        excerpt.push_str(" … [line shortened]");
    }
    (excerpt, shortened)
}

fn finish_grep_match(
    pending: PendingGrepMatch,
    retained: &mut Vec<GrepMatch>,
    output_bytes: &mut usize,
    report: &mut super::SearchReport,
) {
    let cost = pending
        .before
        .iter()
        .chain(&pending.after)
        .map(|line| pending.path.len() + line.text().len() + 32)
        .chain(std::iter::once(
            pending.path.len() + pending.text.len() + 32,
        ))
        .sum::<usize>();
    if output_bytes.saturating_add(cost) <= MAX_SEARCH_OUTPUT_BYTES {
        *output_bytes += cost;
        retained.push(
            GrepMatch::new(pending.path, pending.line, pending.text)
                .with_context(pending.before, pending.after),
        );
    } else {
        report.omitted_rows += 1;
    }
}

fn bounded_diff(text: &str) -> String {
    let mut end = text.len().min(MAX_EDIT_DIFF_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn apply_replacement(
    text: &str,
    spec: &EditFileSpec,
) -> Result<(String, EditFileOutput), FileSystemError> {
    let count = text.matches(spec.old()).count();
    if count == 0 || (!spec.replace_all() && count != 1) {
        return Err(FileSystemError::invalid_match_count(count));
    }
    let removed_bytes = spec
        .old()
        .len()
        .checked_mul(count)
        .ok_or_else(|| FileSystemError::new(FileSystemErrorCode::InvalidSpec))?;
    let inserted_bytes = spec
        .new_text()
        .len()
        .checked_mul(count)
        .ok_or_else(|| FileSystemError::new(FileSystemErrorCode::InvalidSpec))?;
    let final_size = text
        .len()
        .checked_sub(removed_bytes)
        .and_then(|size| size.checked_add(inserted_bytes));
    if final_size.is_none_or(|size| size > MAX_EDIT_BYTES) {
        return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
    }
    let first = text
        .find(spec.old())
        .ok_or_else(|| FileSystemError::invalid_match_count(0))?;
    let match_end = first
        .checked_add(spec.old().len())
        .ok_or_else(|| FileSystemError::new(FileSystemErrorCode::InvalidSpec))?;
    let line_start = text[..first].rfind('\n').map_or(0, |index| index + 1);
    let last_affected = match_end.saturating_sub(1);
    let line_end = if text.as_bytes().get(last_affected) == Some(&b'\n') {
        match_end
    } else {
        text[match_end..]
            .find('\n')
            .map_or(text.len(), |index| match_end + index + 1)
    };
    let removed_span = &text[line_start..line_end];
    let inserted_span = removed_span.replacen(spec.old(), spec.new_text(), 1);
    let mut before_context = text[..line_start]
        .lines()
        .rev()
        .take(3)
        .map(bounded_diff)
        .collect::<Vec<_>>();
    before_context.reverse();
    let after_start = if text[..line_end].ends_with('\n') {
        line_end
    } else {
        (line_end + usize::from(text.as_bytes().get(line_end) == Some(&b'\n'))).min(text.len())
    };
    let after_context = text[after_start..]
        .lines()
        .take(3)
        .map(bounded_diff)
        .collect();
    let context = super::EditContext {
        before: before_context,
        after: after_context,
        removed_lines: removed_span.lines().count(),
        inserted_lines: inserted_span.lines().count(),
    };
    let removed = removed_span.strip_suffix('\n').unwrap_or(removed_span);
    let inserted = inserted_span.strip_suffix('\n').unwrap_or(&inserted_span);
    let diff_truncated =
        count > 1 || removed.len() > MAX_EDIT_DIFF_BYTES || inserted.len() > MAX_EDIT_DIFF_BYTES;
    let removed_line = bounded_diff(removed);
    let inserted_line = bounded_diff(inserted);
    let line = text[..line_start].matches('\n').count() + 1;
    let edited = if spec.replace_all() {
        text.replace(spec.old(), spec.new_text())
    } else {
        text.replacen(spec.old(), spec.new_text(), 1)
    };
    Ok((
        edited,
        EditFileOutput::new(count, line, removed_line, inserted_line)
            .with_diff_truncated(diff_truncated)
            .with_context(context),
    ))
}

fn read_edit_text(input: &mut cap_std::fs::File) -> Result<String, FileSystemError> {
    let mut text = String::new();
    std::io::Read::by_ref(input)
        .take((MAX_EDIT_BYTES + 1) as u64)
        .read_to_string(&mut text)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                FileSystemError::new(FileSystemErrorCode::InvalidUtf8)
            } else {
                capability_error(error)
            }
        })?;
    if text.len() > MAX_EDIT_BYTES {
        return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
    }
    if text.contains('\0') {
        return Err(FileSystemError::new(FileSystemErrorCode::Binary));
    }
    Ok(text)
}

#[derive(Clone)]
struct LocalRoot {
    canonical_path: PathBuf,
    access: FileSystemRootAccess,
    directory: Arc<Dir>,
    identity: Stamp,
}

#[derive(Clone)]
pub(crate) struct LocalFileSystemBackend {
    shutdown: CancellationToken,
    observations: ObservationLog,
    policy: FileSystemPolicy,
    roots: Vec<LocalRoot>,
    before_commit: Option<CommitHook>,
}

impl LocalFileSystemBackend {
    pub(crate) fn new(policy: FileSystemPolicy) -> Result<Self, FileSystemError> {
        Self::build(policy, None)
    }

    fn build(
        policy: FileSystemPolicy,
        before_commit: Option<CommitHook>,
    ) -> Result<Self, FileSystemError> {
        let mut roots = Vec::with_capacity(policy.roots().len());
        let mut effective = Vec::with_capacity(policy.roots().len());
        for declared in policy.roots() {
            let canonical_path =
                std::fs::canonicalize(declared.path()).map_err(|error| super::from_io(&error))?;
            if !canonical_path.is_dir() {
                return Err(FileSystemError::new(FileSystemErrorCode::NotDirectory));
            }
            if roots
                .iter()
                .any(|root: &LocalRoot| root.canonical_path == canonical_path)
            {
                return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
            }
            let directory = Dir::open_ambient_dir(&canonical_path, cap_std::ambient_authority())
                .map_err(|error| super::from_io(&error))?;
            if std::fs::canonicalize(&canonical_path).ok().as_deref()
                != Some(canonical_path.as_path())
            {
                return Err(FileSystemError::new(FileSystemErrorCode::ChangedAtCommit));
            }
            let identity = Stamp::of_dir(&directory)
                .ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
            effective.push(FileSystemRoot::new(
                canonical_path.clone(),
                declared.access(),
            )?);
            roots.push(LocalRoot {
                canonical_path,
                access: declared.access(),
                directory: Arc::new(directory),
                identity,
            });
        }
        Ok(Self {
            shutdown: CancellationToken::new(),
            observations: ObservationLog::default(),
            policy: FileSystemPolicy::new(effective)?,
            roots,
            before_commit,
        })
    }

    #[cfg(test)]
    fn new_with_hook(policy: FileSystemPolicy, hook: CommitHook) -> Result<Self, FileSystemError> {
        Self::build(policy, Some(hook))
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    fn check(&self, cancellation: &CancellationToken) -> Result<(), FileSystemError> {
        if self.shutdown.is_cancelled() {
            Err(FileSystemError::new(FileSystemErrorCode::ServiceStopped))
        } else if cancellation.is_cancelled() {
            Err(FileSystemError::new(FileSystemErrorCode::Cancelled))
        } else {
            Ok(())
        }
    }

    fn target<'a>(
        &'a self,
        path: &'a ResolvedPath,
        write: bool,
    ) -> Result<LocalTarget<'a>, FileSystemError> {
        // Raw segments, not `components()`. `Path::components` *normalises `.`
        // away* — `/ws/./f` yields RootDir, Normal("ws"), Normal("f") — so the
        // `Component::CurDir` arm this check used to rely on could never match
        // and only `..` was ever rejected. A `ResolvedPath` is canonical by
        // construction, so one arriving with a dot segment was not produced by
        // `resolve`, and this boundary is where that is caught rather than
        // assumed.
        if contains_dot_segment(path.as_path()) {
            return Err(FileSystemError::new(FileSystemErrorCode::PathTraversal));
        }
        // Selection is by specificity ALONE; access is checked afterwards on
        // the root that won. Filtering read-only roots out of the candidate set
        // first — as this loop used to — let a write to a read-only subtree
        // fall through to its read-write parent and succeed, because skipping
        // the more specific root simply handed the match to the less specific
        // one. The nested grant is the whole point of declaring it, so the most
        // specific root must win the match and then be obeyed.
        let mut selected: Option<(&LocalRoot, PathBuf)> = None;
        for root in &self.roots {
            let Ok(relative) = path.as_path().strip_prefix(&root.canonical_path) else {
                continue;
            };
            if !safe_relative_path(relative) {
                return Err(FileSystemError::new(FileSystemErrorCode::PathTraversal));
            }
            let replace = selected.as_ref().is_none_or(|(current, _)| {
                root.canonical_path.components().count()
                    > current.canonical_path.components().count()
            });
            if replace {
                selected = Some((root, relative.to_path_buf()));
            }
        }
        let Some((root, relative)) = selected else {
            return Err(FileSystemError::new(
                FileSystemErrorCode::OutsideAllowedRoots,
            ));
        };
        if write && !root.access.permits_write() {
            return Err(FileSystemError::new(FileSystemErrorCode::ReadOnlyRoot));
        }
        Ok(LocalTarget {
            root,
            relative,
            absolute: path.as_path(),
        })
    }

    fn collect_files(
        &self,
        base: &Dir,
        ignored: &[OsString],
        cancellation: &CancellationToken,
        report: &mut super::SearchReport,
    ) -> Result<Vec<PathBuf>, FileSystemError> {
        let mut files = Vec::new();
        let mut stack = vec![PathBuf::new()];
        let mut entry_count = 0;
        let mut path_bytes = 0;
        'walk: while let Some(relative_directory) = stack.pop() {
            self.check(cancellation)?;
            let directory = match open_relative_dir(base, &relative_directory) {
                Ok(directory) => directory,
                Err(error) if relative_directory.as_os_str().is_empty() => return Err(error),
                Err(_) => {
                    report.unreadable += 1;
                    continue;
                }
            };
            let entries = match directory.entries() {
                Ok(entries) => entries,
                Err(error) if relative_directory.as_os_str().is_empty() => {
                    return Err(capability_error(error));
                }
                Err(_) => {
                    report.unreadable += 1;
                    continue;
                }
            };
            // Never collect an unbounded directory iterator into memory.
            for entry in entries {
                self.check(cancellation)?;
                entry_count += 1;
                if entry_count > MAX_SEARCH_ENTRIES {
                    report.budget_exhausted = true;
                    break 'walk;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => {
                        report.unreadable += 1;
                        continue;
                    }
                };
                let name = entry.file_name();
                let Some(name_text) = name.to_str() else {
                    report.non_utf8 += 1;
                    continue;
                };
                if name_text.contains(['\\', ':']) || name_text.chars().any(char::is_control) {
                    report.unsupported_paths += 1;
                    continue;
                }
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(_) => {
                        report.unreadable += 1;
                        continue;
                    }
                };
                if !file_type.is_dir() && !file_type.is_file() {
                    continue;
                }
                if file_type.is_dir() && ignored.iter().any(|ignored| ignored == &name) {
                    continue;
                }
                let relative = relative_directory.join(&name);
                path_bytes += relative.as_os_str().len();
                if path_bytes > MAX_SEARCH_PATH_BYTES {
                    report.budget_exhausted = true;
                    break 'walk;
                }
                if file_type.is_dir() {
                    stack.push(relative);
                } else {
                    files.push(relative);
                }
            }
        }
        // Publish a sorted observed subset even when traversal is incomplete.
        files.sort_by_cached_key(|path| normalized_display(path));
        Ok(files)
    }

    fn secure_atomic_write(
        &self,
        target: &LocalTarget<'_>,
        bytes: &[u8],
        expected: Option<Stamp>,
        permissions: Option<Permissions>,
        cancellation: &CancellationToken,
    ) -> Result<Stamp, FileSystemError> {
        let (parent_relative, file_name) = split_parent(&target.relative)?;
        // Before touching the retained directory handle. If the root was
        // unlinked and replaced, every operation through that handle fails
        // with a bare ENOENT, and the caller learns "not found" about a path
        // that plainly exists — the substitution, which is the actionable
        // fact, is exactly what the generic error hides.
        verify_root_identity(target.root)?;
        target
            .root
            .directory
            .create_dir_all(&parent_relative)
            .map_err(capability_error)?;
        let parent = open_relative_dir(&target.root.directory, &parent_relative)?;
        let parent_stamp =
            Stamp::of_dir(&parent).ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
        let temporary_name = OsString::from(format!(".heycode-{}.tmp", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut temporary = parent
            .open_with(&temporary_name, &options)
            .map_err(capability_error)?;
        let guard = TemporaryEntry {
            parent: &parent,
            name: &temporary_name,
        };
        if let Some(permissions) = permissions {
            temporary
                .set_permissions(permissions)
                .map_err(capability_error)?;
        }
        temporary.write_all(bytes).map_err(capability_error)?;
        temporary.sync_all().map_err(capability_error)?;
        let prepared_stamp = Stamp::of_file(&temporary)
            .ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
        drop(temporary);

        if let Some(hook) = &self.before_commit {
            hook(target.absolute);
        }
        self.check(cancellation)?;
        // Re-checked here as well as before the write: the root can be
        // swapped while the temporary file is being written, and the commit is
        // the moment that matters.
        verify_root_identity(target.root)?;
        let reopened_parent = open_relative_dir(&target.root.directory, &parent_relative)
            .map_err(|_| FileSystemError::new(FileSystemErrorCode::ChangedAtCommit))?;
        if !Stamp::of_dir(&reopened_parent)
            .is_some_and(|reopened| parent_stamp.same_identity(reopened))
        {
            return Err(FileSystemError::new(FileSystemErrorCode::ChangedAtCommit));
        }

        if let Some(expected) = expected {
            let current = parent
                .open(&file_name)
                .map_err(|_| FileSystemError::new(FileSystemErrorCode::ChangedAtCommit))?;
            if Stamp::of_file(&current) != Some(expected)
                || !self.observations.is_fresh_opened(target.absolute, &current)
            {
                return Err(FileSystemError::new(FileSystemErrorCode::StaleObservation));
            }
            drop(current);
            self.check(cancellation)?;
            parent
                .rename(&temporary_name, &parent, &file_name)
                .map_err(capability_error)?;
        } else {
            self.check(cancellation)?;
            match parent.hard_link(&temporary_name, &parent, &file_name) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(FileSystemError::new(FileSystemErrorCode::ChangedAtCommit));
                }
                Err(error) => return Err(capability_error(error)),
            }
        }
        drop(guard);
        let committed_stamp = parent
            .open(&file_name)
            .ok()
            .and_then(|file| Stamp::of_file(&file))
            .filter(|stamp| prepared_stamp.same_identity(*stamp))
            .unwrap_or(prepared_stamp);
        #[cfg(unix)]
        {
            let _ = parent
                .try_clone()
                .map(Dir::into_std_file)
                .and_then(|directory| directory.sync_all());
        }
        Ok(committed_stamp)
    }
}

#[async_trait]
impl FileSystemBackend for LocalFileSystemBackend {
    fn resolve(&self, request: PathRequest) -> Result<ResolvedPath, FileSystemError> {
        if self.shutdown.is_cancelled() {
            return Err(FileSystemError::new(FileSystemErrorCode::ServiceStopped));
        }
        let candidate = if request.path().is_absolute() {
            request.path().to_path_buf()
        } else {
            request.cwd().join(request.path())
        };
        if candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(FileSystemError::new(FileSystemErrorCode::PathTraversal));
        }
        let canonical = canonicalize_nearest(&remove_current_components(&candidate))?;
        let resolved = ResolvedPath::new(canonical)?;
        self.target(&resolved, false)?;
        Ok(resolved)
    }

    fn observations(&self) -> ObservationLog {
        self.observations.clone()
    }

    fn policy(&self) -> FileSystemPolicy {
        self.policy.clone()
    }

    async fn metadata(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<FileMetadata, FileSystemError> {
        self.check(&cancellation)?;
        let target = self.target(&path, false)?;
        let metadata = relative_metadata(&target.root.directory, &target.relative)?;
        self.check(&cancellation)?;
        let kind = if metadata.is_file() {
            FileEntryKind::File
        } else if metadata.is_dir() {
            FileEntryKind::Directory
        } else {
            FileEntryKind::Other
        };
        Ok(FileMetadata::new(kind, metadata.len()))
    }

    async fn create_dir_all(
        &self,
        path: ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError> {
        self.check(&cancellation)?;
        let target = self.target(&path, true)?;
        target
            .root
            .directory
            .create_dir_all(&target.relative)
            .map_err(capability_error)
    }

    async fn read(
        &self,
        spec: ReadFileSpec,
        cancellation: CancellationToken,
    ) -> Result<ReadFileOutput, FileSystemError> {
        self.check(&cancellation)?;
        let target = self.target(spec.path(), false)?;
        let mut input = target
            .root
            .directory
            .open(&target.relative)
            .map_err(capability_error)?;
        let metadata = input.metadata().map_err(capability_error)?;
        if !metadata.is_file() {
            return Err(FileSystemError::new(FileSystemErrorCode::NotFile));
        }
        let before =
            Stamp::of_file(&input).ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
        if spec
            .expected_revision()
            .is_some_and(|revision| revision != before.revision())
        {
            return Err(FileSystemError::new(FileSystemErrorCode::StaleObservation));
        }
        if let Some(window) = spec.window().copied() {
            let max_bytes = spec.max_bytes();
            let stopped = self.shutdown.clone();
            let cancelled = cancellation.clone();
            let (output, after) = tokio::task::spawn_blocking(move || {
                super::text_page::read_page(input, window, max_bytes, cancelled, stopped)
            })
            .await
            .map_err(|_| FileSystemError::new(FileSystemErrorCode::Io))??;
            self.check(&cancellation)?;
            if before != after {
                return Err(FileSystemError::new(FileSystemErrorCode::ChangedAtCommit));
            }
            self.observations.mark_stamp(target.absolute, after);
            return Ok(output);
        }
        let retained_and_probe = spec.max_bytes().saturating_add(1).max(8192);
        let mut bytes = Vec::with_capacity(retained_and_probe.min(1024 * 1024));
        std::io::Read::by_ref(&mut input)
            .take(u64::try_from(retained_and_probe).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(capability_error)?;
        self.check(&cancellation)?;
        if !spec.allows_binary() && looks_binary(&bytes) {
            return Err(FileSystemError::new(FileSystemErrorCode::Binary));
        }
        let after =
            Stamp::of_file(&input).ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
        if before != after {
            return Err(FileSystemError::new(FileSystemErrorCode::ChangedAtCommit));
        }
        let truncated = bytes.len() > spec.max_bytes();
        bytes.truncate(spec.max_bytes());
        self.observations.mark_stamp(target.absolute, after);
        Ok(ReadFileOutput::new(bytes, truncated))
    }

    async fn write(
        &self,
        spec: WriteFileSpec,
        cancellation: CancellationToken,
    ) -> Result<(), FileSystemError> {
        self.check(&cancellation)?;
        let target = self.target(spec.path(), true)?;
        let current = match target.root.directory.open(&target.relative) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(capability_error(error)),
        };
        let expected = current
            .as_ref()
            .map(|file| {
                Stamp::of_file(file).ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))
            })
            .transpose()?;
        let permissions = if let Some(current) = &current {
            if !self.observations.contains_exact(target.absolute) {
                return Err(FileSystemError::new(FileSystemErrorCode::NotObserved));
            }
            if !self.observations.is_fresh_opened(target.absolute, current) {
                return Err(FileSystemError::new(FileSystemErrorCode::StaleObservation));
            }
            let metadata = current.metadata().map_err(capability_error)?;
            if !metadata.is_file() {
                return Err(FileSystemError::new(FileSystemErrorCode::NotFile));
            }
            Some(metadata.permissions())
        } else {
            None
        };
        drop(current);
        let committed_stamp =
            self.secure_atomic_write(&target, spec.bytes(), expected, permissions, &cancellation)?;
        self.observations
            .mark_stamp(target.absolute, committed_stamp);
        Ok(())
    }

    async fn edit(
        &self,
        spec: EditFileSpec,
        cancellation: CancellationToken,
    ) -> Result<EditFileOutput, FileSystemError> {
        let result = self
            .edit_many(super::MultiEditSpec::new(vec![spec])?, cancellation)
            .await?;
        result
            .edits
            .into_iter()
            .next()
            .ok_or_else(|| FileSystemError::new(FileSystemErrorCode::InvalidOutput))
    }

    async fn edit_many(
        &self,
        spec: super::MultiEditSpec,
        cancellation: CancellationToken,
    ) -> Result<super::MultiEditOutput, FileSystemError> {
        self.check(&cancellation)?;
        let path = spec
            .edits()
            .first()
            .ok_or_else(|| FileSystemError::new(FileSystemErrorCode::InvalidSpec))?
            .path();
        let target = self.target(path, true)?;
        let mut input = target
            .root
            .directory
            .open(&target.relative)
            .map_err(capability_error)?;
        if !self.observations.contains_exact(target.absolute) {
            return Err(FileSystemError::new(FileSystemErrorCode::NotObserved));
        }
        if !self.observations.is_fresh_opened(target.absolute, &input) {
            return Err(FileSystemError::new(FileSystemErrorCode::StaleObservation));
        }
        let before =
            Stamp::of_file(&input).ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
        if spec
            .expected_revision()
            .is_some_and(|revision| revision != before.revision())
        {
            return Err(FileSystemError::new(FileSystemErrorCode::StaleObservation));
        }
        let metadata = input.metadata().map_err(capability_error)?;
        if !metadata.is_file() {
            return Err(FileSystemError::new(FileSystemErrorCode::NotFile));
        }
        if metadata.len() > MAX_EDIT_BYTES as u64 {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
        }
        let original = read_edit_text(&mut input)?;
        let mut text = original.clone();
        let mut edits = Vec::with_capacity(spec.edits().len());
        for (index, edit) in spec.edits().iter().enumerate() {
            self.check(&cancellation)?;
            let (next, output) =
                apply_replacement(&text, edit).map_err(|error| error.at_edit(index + 1))?;
            text = next;
            edits.push(output);
        }
        if Stamp::of_file(&input) != Some(before) {
            return Err(FileSystemError::new(FileSystemErrorCode::StaleObservation));
        }
        self.check(&cancellation)?;
        let changed = text != original;
        drop(input);
        let after = if spec.dry_run() || !changed {
            before
        } else {
            let committed = self.secure_atomic_write(
                &target,
                text.as_bytes(),
                Some(before),
                Some(metadata.permissions()),
                &cancellation,
            )?;
            self.observations.mark_stamp(target.absolute, committed);
            committed
        };
        Ok(super::MultiEditOutput {
            edits,
            revision: after.revision(),
            dry_run: spec.dry_run(),
            changed,
        })
    }

    async fn write_checked(
        &self,
        spec: super::CheckedWriteSpec,
        cancellation: CancellationToken,
    ) -> Result<super::CheckedWriteOutput, FileSystemError> {
        self.check(&cancellation)?;
        let write = spec.write();
        let target = self.target(write.path(), true)?;
        let current = match target.root.directory.open(&target.relative) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(capability_error(error)),
        };
        let (expected, permissions) = match (current, spec.expected_revision()) {
            (Some(_), None) => {
                return Err(FileSystemError::new(FileSystemErrorCode::ChangedAtCommit));
            }
            (None, Some(_)) => return Err(FileSystemError::new(FileSystemErrorCode::NotFound)),
            (None, None) => (None, None),
            (Some(mut current), Some(revision)) => {
                if !self.observations.contains_exact(target.absolute) {
                    return Err(FileSystemError::new(FileSystemErrorCode::NotObserved));
                }
                let before = Stamp::of_file(&current)
                    .ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
                if before.revision() != revision
                    || !self.observations.is_fresh_opened(target.absolute, &current)
                {
                    return Err(FileSystemError::new(FileSystemErrorCode::StaleObservation));
                }
                let metadata = current.metadata().map_err(capability_error)?;
                if !metadata.is_file() {
                    return Err(FileSystemError::new(FileSystemErrorCode::NotFile));
                }
                if metadata.len() == write.bytes().len() as u64 {
                    let mut buffer = [0_u8; 8192];
                    let mut offset = 0;
                    let mut same = true;
                    while offset < write.bytes().len() {
                        self.check(&cancellation)?;
                        let count = current.read(&mut buffer).map_err(capability_error)?;
                        if count == 0
                            || write.bytes().get(offset..offset + count) != Some(&buffer[..count])
                        {
                            same = false;
                            break;
                        }
                        offset += count;
                    }
                    if Stamp::of_file(&current) != Some(before) {
                        return Err(FileSystemError::new(FileSystemErrorCode::StaleObservation));
                    }
                    if same {
                        return Ok(super::CheckedWriteOutput {
                            revision: before.revision(),
                            changed: false,
                            bytes: write.bytes().len(),
                        });
                    }
                }
                (Some(before), Some(metadata.permissions()))
            }
        };
        let after =
            self.secure_atomic_write(&target, write.bytes(), expected, permissions, &cancellation)?;
        self.observations.mark_stamp(target.absolute, after);
        Ok(super::CheckedWriteOutput {
            revision: after.revision(),
            changed: true,
            bytes: write.bytes().len(),
        })
    }

    async fn glob(
        &self,
        spec: GlobSpec,
        cancellation: CancellationToken,
    ) -> Result<GlobOutput, FileSystemError> {
        let backend = self.clone();
        let worker_cancellation = cancellation.child_token();
        let _cancel_on_drop = worker_cancellation.clone().drop_guard();
        tokio::task::spawn_blocking(move || backend.glob_blocking(spec, worker_cancellation))
            .await
            .map_err(|_| FileSystemError::new(FileSystemErrorCode::Io))?
    }

    async fn grep(
        &self,
        spec: GrepSpec,
        cancellation: CancellationToken,
    ) -> Result<GrepOutput, FileSystemError> {
        let backend = self.clone();
        let worker_cancellation = cancellation.child_token();
        let _cancel_on_drop = worker_cancellation.clone().drop_guard();
        tokio::task::spawn_blocking(move || backend.grep_blocking(spec, worker_cancellation))
            .await
            .map_err(|_| FileSystemError::new(FileSystemErrorCode::Io))?
    }
}

impl LocalFileSystemBackend {
    fn glob_blocking(
        &self,
        spec: GlobSpec,
        cancellation: CancellationToken,
    ) -> Result<GlobOutput, FileSystemError> {
        self.check(&cancellation)?;
        let target = self.target(spec.root(), false)?;
        let directory = open_relative_dir(&target.root.directory, &target.relative)?;
        let mut report = super::SearchReport::default();
        let files = self.collect_files(
            &directory,
            spec.ignored_directories(),
            &cancellation,
            &mut report,
        )?;
        let mut retained = Vec::new();
        let mut total = 0_usize;
        let mut output_bytes = 0_usize;
        for relative in files {
            self.check(&cancellation)?;
            let components = component_strings(&relative);
            if glob_match(spec.pattern(), &components) {
                total += 1;
                if retained.len() < spec.result_limit() {
                    let shown = normalized_display(&relative);
                    if output_bytes + shown.len() < MAX_SEARCH_OUTPUT_BYTES {
                        output_bytes += shown.len() + 1;
                        retained.push(shown);
                    } else {
                        report.omitted_rows += 1;
                    }
                }
            }
        }
        Ok(GlobOutput::new(retained, total).with_report(report))
    }

    fn grep_blocking(
        &self,
        spec: GrepSpec,
        cancellation: CancellationToken,
    ) -> Result<GrepOutput, FileSystemError> {
        self.check(&cancellation)?;
        let expression = RegexBuilder::new(spec.pattern())
            .case_insensitive(spec.case_insensitive())
            .build()
            .map_err(|_| FileSystemError::new(FileSystemErrorCode::Pattern))?;
        let target = self.target(spec.root(), false)?;
        let metadata = relative_metadata(&target.root.directory, &target.relative)?;
        let mut report = super::SearchReport::default();
        let (base, files) = if metadata.is_file() {
            let (parent, name) = split_parent(&target.relative)?;
            (
                open_relative_dir(&target.root.directory, &parent)?,
                vec![PathBuf::from(name)],
            )
        } else if metadata.is_dir() {
            let base = open_relative_dir(&target.root.directory, &target.relative)?;
            let files = self.collect_files(
                &base,
                spec.ignored_directories(),
                &cancellation,
                &mut report,
            )?;
            (base, files)
        } else {
            return Err(FileSystemError::new(FileSystemErrorCode::NotFile));
        };

        let mut retained = Vec::new();
        let mut retained_files = Vec::new();
        let mut total = 0_usize;
        let mut total_files = 0_usize;
        let mut selected_content = 0_usize;
        let mut selected_files = 0_usize;
        let mut output_bytes = 0_usize;
        let mut scanned_bytes = 0_u64;
        for relative in files {
            self.check(&cancellation)?;
            if scanned_bytes >= MAX_SEARCH_TOTAL_BYTES {
                report.budget_exhausted = true;
                break;
            }
            let Some(relative_text) = relative.to_str() else {
                report.non_utf8 += 1;
                continue;
            };
            if relative_text.contains(['\\', ':']) || relative_text.chars().any(char::is_control) {
                report.unsupported_paths += 1;
                continue;
            }
            if let Some(include) = spec.include() {
                let include_matches = if include.contains('/') {
                    let components = component_strings(&relative);
                    glob_match(include, &components)
                } else {
                    let name = relative
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    glob_match_one(include, name)
                };
                if !include_matches {
                    continue;
                }
            }
            let input = match base.open(&relative) {
                Ok(input) => input,
                Err(_) => {
                    report.unreadable += 1;
                    continue;
                }
            };
            let mut reader = std::io::BufReader::with_capacity(8192, input);
            match reader.fill_buf() {
                Ok(probe) if looks_binary(probe) => {
                    scanned_bytes += probe.len() as u64;
                    continue;
                }
                Ok(_) => {}
                Err(_) => {
                    report.unreadable += 1;
                    continue;
                }
            }
            let shown_path = normalized_display(&relative);
            let mut line = Vec::new();
            let mut oversized = false;
            let mut line_number = 1;
            let mut file_bytes = 0_u64;
            let mut file_matches = 0_usize;
            let mut before = VecDeque::<Option<(usize, String, bool)>>::new();
            let mut pending = VecDeque::<PendingGrepMatch>::new();
            loop {
                self.check(&cancellation)?;
                let available = match reader.fill_buf() {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        report.unreadable += 1;
                        break;
                    }
                };
                let eof = available.is_empty();
                if !eof
                    && (file_bytes >= MAX_SEARCH_FILE_BYTES
                        || scanned_bytes >= MAX_SEARCH_TOTAL_BYTES)
                {
                    if file_bytes >= MAX_SEARCH_FILE_BYTES {
                        report.limited_files += 1;
                    }
                    if scanned_bytes >= MAX_SEARCH_TOTAL_BYTES {
                        report.budget_exhausted = true;
                    }
                    break;
                }
                let count = available
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(available.len(), |index| index + 1)
                    .min((MAX_SEARCH_FILE_BYTES - file_bytes) as usize)
                    .min((MAX_SEARCH_TOTAL_BYTES - scanned_bytes) as usize);
                let complete_line = count > 0 && available[count - 1] == b'\n';
                if !oversized {
                    if line.len() + count > MAX_SEARCH_LINE_BYTES {
                        oversized = true;
                        line.clear();
                        report.oversized_lines += 1;
                    } else {
                        line.extend_from_slice(&available[..count]);
                    }
                }
                reader.consume(count);
                file_bytes += count as u64;
                scanned_bytes += count as u64;
                if complete_line || (eof && !line.is_empty()) {
                    let mut context_line = None;
                    let mut matches = false;
                    if !oversized {
                        match std::str::from_utf8(&line) {
                            Ok(text) => {
                                let text = text.strip_suffix('\n').unwrap_or(text);
                                let text = text.strip_suffix('\r').unwrap_or(text);
                                if text.chars().any(|ch| ch.is_control() && ch != '\t') {
                                    report.non_text_lines += 1;
                                } else {
                                    matches = expression.is_match(text);
                                    let (excerpt, shortened) = bounded_search_excerpt(text);
                                    context_line = Some((line_number, excerpt, shortened));
                                }
                            }
                            Err(_) => report.non_utf8 += 1,
                        }
                    }

                    for candidate in &mut pending {
                        if candidate.after_seen < candidate.after_limit {
                            candidate.after_seen += 1;
                            if let Some((line, text, shortened)) = &context_line {
                                candidate
                                    .after
                                    .push(GrepContextLine::new(*line, text.clone()));
                                if *shortened {
                                    report.shortened_excerpts += 1;
                                }
                            }
                        }
                    }
                    while pending
                        .front()
                        .is_some_and(|candidate| candidate.after_seen == candidate.after_limit)
                    {
                        let Some(candidate) = pending.pop_front() else {
                            break;
                        };
                        finish_grep_match(candidate, &mut retained, &mut output_bytes, &mut report);
                    }

                    if matches {
                        let match_index = total;
                        total += 1;
                        file_matches += 1;
                        if spec.output_mode() == GrepOutputMode::Content
                            && match_index >= spec.offset()
                            && selected_content < spec.result_limit()
                        {
                            selected_content += 1;
                            let Some((line, text, shortened)) = context_line.as_ref() else {
                                return Err(FileSystemError::new(
                                    FileSystemErrorCode::InvalidOutput,
                                ));
                            };
                            if *shortened {
                                report.shortened_excerpts += 1;
                            }
                            let prior = before
                                .iter()
                                .flatten()
                                .map(|(line, text, shortened)| {
                                    if *shortened {
                                        report.shortened_excerpts += 1;
                                    }
                                    GrepContextLine::new(*line, text.clone())
                                })
                                .collect();
                            let candidate = PendingGrepMatch {
                                path: shown_path.clone(),
                                line: *line,
                                text: text.clone(),
                                before: prior,
                                after: Vec::new(),
                                after_limit: spec.after_context(),
                                after_seen: 0,
                            };
                            if candidate.after_limit == 0 {
                                finish_grep_match(
                                    candidate,
                                    &mut retained,
                                    &mut output_bytes,
                                    &mut report,
                                );
                            } else {
                                pending.push_back(candidate);
                            }
                        }
                    }

                    if spec.before_context() > 0 {
                        before.push_back(context_line);
                        while before.len() > spec.before_context() {
                            before.pop_front();
                        }
                    }
                    line.clear();
                    oversized = false;
                    line_number += 1;
                }
                if eof {
                    break;
                }
            }
            while let Some(candidate) = pending.pop_front() {
                finish_grep_match(candidate, &mut retained, &mut output_bytes, &mut report);
            }
            if file_matches > 0 {
                let file_index = total_files;
                total_files += 1;
                if spec.output_mode() != GrepOutputMode::Content
                    && file_index >= spec.offset()
                    && selected_files < spec.result_limit()
                {
                    selected_files += 1;
                    let cost = shown_path.len() + 32;
                    if output_bytes.saturating_add(cost) <= MAX_SEARCH_OUTPUT_BYTES {
                        output_bytes += cost;
                        retained_files.push(GrepFileMatch::new(shown_path, file_matches));
                    } else {
                        report.omitted_rows += 1;
                    }
                }
            }
        }
        let published_total_files = if spec.output_mode() == GrepOutputMode::Content {
            0
        } else {
            total_files
        };
        Ok(GrepOutput::new(retained, total)
            .with_files(retained_files, published_total_files)
            .with_report(report))
    }
}

struct LocalTarget<'a> {
    root: &'a LocalRoot,
    relative: PathBuf,
    absolute: &'a Path,
}

struct TemporaryEntry<'a> {
    parent: &'a Dir,
    name: &'a OsStr,
}

impl Drop for TemporaryEntry<'_> {
    fn drop(&mut self) {
        let _ = self.parent.remove_file(self.name);
    }
}

fn canonicalize_nearest(path: &Path) -> Result<PathBuf, FileSystemError> {
    let mut probe = path.to_path_buf();
    let mut missing = Vec::<OsString>::new();
    loop {
        match std::fs::canonicalize(&probe) {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = probe.file_name().map(OsStr::to_os_string) else {
                    return Err(super::from_io(&error));
                };
                missing.push(name);
                if !probe.pop() {
                    return Err(super::from_io(&error));
                }
            }
            Err(error) => return Err(super::from_io(&error)),
        }
    }
}

/// Whether any raw segment of `path` is `.` or `..`.
///
/// Deliberately string-based: `Path::components()` silently drops `.`, so a
/// component-level check cannot see it. Splitting on both separators keeps the
/// answer the same on Windows, where either may appear.
/// Whether a root still is the directory the service opened at construction.
///
/// Identity, not path: a root removed and replaced by a decoy keeps its name
/// and canonical path while becoming a different directory, so only the stamp
/// taken when the service was built can tell them apart.
fn verify_root_identity(root: &LocalRoot) -> Result<(), FileSystemError> {
    let reopened = Dir::open_ambient_dir(&root.canonical_path, cap_std::ambient_authority())
        .map_err(|_| FileSystemError::new(FileSystemErrorCode::ChangedAtCommit))?;
    if Stamp::of_dir(&reopened).is_some_and(|now| root.identity.same_identity(now)) {
        Ok(())
    } else {
        Err(FileSystemError::new(FileSystemErrorCode::ChangedAtCommit))
    }
}

fn contains_dot_segment(path: &Path) -> bool {
    path.to_string_lossy()
        .split(['/', '\\'])
        .any(|segment| segment == "." || segment == "..")
}

fn safe_relative_path(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return true;
    }
    path.components().all(|component| match component {
        Component::Normal(value) => {
            #[cfg(windows)]
            {
                windows_component_is_safe(value)
            }
            #[cfg(not(windows))]
            {
                let _ = value;
                true
            }
        }
        _ => false,
    })
}

#[cfg(any(windows, test))]
fn windows_component_is_safe(value: &OsStr) -> bool {
    let Some(value) = value.to_str() else {
        return false;
    };
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.ends_with([' ', '.'])
        || value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
    {
        return false;
    }

    let stem = value
        .split_once('.')
        .map_or(value, |(stem, _extension)| stem)
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "CLOCK$" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !windows_numbered_device(&stem)
}

#[cfg(any(windows, test))]
fn windows_numbered_device(stem: &str) -> bool {
    let Some(suffix) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut characters = suffix.chars();
    characters.next().is_some_and(|digit| {
        characters.next().is_none() && matches!(digit, '1'..='9' | '¹' | '²' | '³')
    })
}

fn open_relative_dir(root: &Dir, relative: &Path) -> Result<Dir, FileSystemError> {
    if relative.as_os_str().is_empty() {
        root.try_clone().map_err(capability_error)
    } else {
        root.open_dir(relative).map_err(capability_error)
    }
}

fn relative_metadata(
    root: &Dir,
    relative: &Path,
) -> Result<cap_std::fs::Metadata, FileSystemError> {
    if relative.as_os_str().is_empty() {
        root.dir_metadata().map_err(capability_error)
    } else {
        root.metadata(relative).map_err(capability_error)
    }
}

fn split_parent(path: &Path) -> Result<(PathBuf, OsString), FileSystemError> {
    let Some(name) = path.file_name().map(OsStr::to_os_string) else {
        return Err(FileSystemError::new(FileSystemErrorCode::NotFile));
    };
    let parent = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    Ok((parent, name))
}

fn capability_error(error: std::io::Error) -> FileSystemError {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput => {
            FileSystemError::new(FileSystemErrorCode::PathTraversal)
        }
        std::io::ErrorKind::NotADirectory => {
            FileSystemError::new(FileSystemErrorCode::NotDirectory)
        }
        _ => super::from_io(&error),
    }
}

fn looks_binary(bytes: &[u8]) -> bool {
    let probe = &bytes[..bytes.len().min(8192)];
    if probe.is_empty() {
        return false;
    }
    let nuls = probe.iter().filter(|&&byte| byte == 0).count();
    nuls * 10 > probe.len()
}

fn normalized_display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn component_strings(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect()
}

fn glob_match(pattern: &str, components: &[&str]) -> bool {
    match_segments(&pattern.split('/').collect::<Vec<_>>(), components)
}

fn match_segments(pattern: &[&str], components: &[&str]) -> bool {
    let mut reachable = vec![false; components.len() + 1];
    reachable[0] = true;
    for segment in pattern {
        let mut next = vec![false; components.len() + 1];
        if *segment == "**" {
            next[0] = reachable[0];
            for index in 1..=components.len() {
                next[index] = reachable[index] || next[index - 1];
            }
        } else {
            for index in 1..=components.len() {
                next[index] =
                    reachable[index - 1] && glob_match_one(segment, components[index - 1]);
            }
        }
        reachable = next;
    }
    reachable[components.len()]
}

fn glob_match_one(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    match_chars(&pattern, &text)
}

fn match_chars(pattern: &[char], text: &[char]) -> bool {
    let mut pattern_index = 0_usize;
    let mut text_index = 0_usize;
    let mut last_star = None;
    let mut star_match = 0_usize;
    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern.get(pattern_index) == Some(&'*') {
            last_star = Some(pattern_index);
            pattern_index += 1;
            star_match = text_index;
        } else if let Some(star) = last_star {
            star_match += 1;
            text_index = star_match;
            pattern_index = star + 1;
        } else {
            return false;
        }
    }
    while pattern.get(pattern_index) == Some(&'*') {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, Ordering};

    fn policy(root: &Path) -> FileSystemPolicy {
        FileSystemPolicy::new([FileSystemRoot::new(root, FileSystemRootAccess::ReadWrite).unwrap()])
            .unwrap()
    }

    fn resolve(service: &crate::FileSystemService, root: &Path, path: &Path) -> ResolvedPath {
        service
            .resolve(PathRequest::new(root, path).unwrap())
            .unwrap()
    }

    #[test]
    fn edit_context_preserves_line_boundaries_for_insertions_and_deletions() {
        let root = tempfile::tempdir().unwrap();
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let path = resolve(&service, root.path(), &root.path().join("sample.txt"));
        for (old, new, expected, removed, added, after) in [
            ("beta", "delta", "alpha\ndelta\ngamma\n", 1, 1, "gamma"),
            ("beta", "\n", "alpha\n\n\ngamma\n", 1, 2, "gamma"),
            ("beta\n", "", "alpha\ngamma\n", 1, 0, "gamma"),
        ] {
            let spec = EditFileSpec::new(path.clone(), old, new, false).unwrap();
            let (text, output) = apply_replacement("alpha\nbeta\ngamma\n", &spec).unwrap();
            assert_eq!(text, expected);
            let context = output.context().unwrap();
            assert_eq!(context.before, ["alpha"]);
            assert_eq!(context.after, [after]);
            assert_eq!(context.removed_lines, removed);
            assert_eq!(context.inserted_lines, added);
        }
    }

    #[tokio::test]
    async fn grep_streams_oversized_and_non_utf8_lines_without_losing_later_line_numbers() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = b"needle first\r\n".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', MAX_SEARCH_LINE_BYTES + 9));
        bytes.extend_from_slice(b"needle\nneedle \xff\nneedle last");
        std::fs::write(root.path().join("large.txt"), bytes).unwrap();
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let output = service
            .grep(
                GrepSpec::new(
                    resolve(&service, root.path(), root.path()),
                    "needle",
                    None,
                    ["target"],
                    200,
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output.total_matches(), 2);
        assert_eq!(
            output
                .matches()
                .iter()
                .map(GrepMatch::line)
                .collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert_eq!(output.matches()[0].text(), "needle first");
        assert_eq!(output.matches()[1].text(), "needle last");
        assert_eq!(output.report().oversized_lines, 1);
        assert_eq!(output.report().non_utf8, 1);
        assert!(output.report().incomplete());
        assert!(output.report().notice().unwrap().contains("lower bound"));
    }

    #[tokio::test]
    async fn grep_bounds_excerpts_and_aggregate_output_without_changing_observed_total() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("long.txt"),
            format!("needle{}\n", "é".repeat(3000)).repeat(200),
        )
        .unwrap();
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let output = service
            .grep(
                GrepSpec::new(
                    resolve(&service, root.path(), root.path()),
                    "needle",
                    None,
                    ["target"],
                    200,
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output.total_matches(), 200);
        assert!(!output.report().incomplete());
        assert!(output.report().shortened_excerpts > 0);
        assert!(output.report().omitted_rows > 0);
        assert!(
            output
                .matches()
                .iter()
                .map(|entry| entry.path().len() + entry.text().len() + 8)
                .sum::<usize>()
                < MAX_SEARCH_OUTPUT_BYTES
        );
        assert!(
            output
                .matches()
                .iter()
                .all(|entry| entry.text().ends_with("[line shortened]"))
        );
    }

    #[tokio::test]
    async fn grep_supports_modes_offset_context_case_and_path_globs() {
        let root = tempfile::tempdir().unwrap();
        for (path, body) in [
            ("src/a.txt", "zero\nNeedle one\nbetween\nneedle two\ntail\n"),
            ("src/deep/b.txt", "NEEDLE three\n"),
            ("other.txt", "needle excluded\n"),
        ] {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let resolved = resolve(&service, root.path(), root.path());

        let content = service
            .grep(
                GrepSpec::new(
                    resolved.clone(),
                    "needle",
                    Some("src/**/*.txt"),
                    ["target"],
                    1,
                )
                .unwrap()
                .with_options(GrepOutputMode::Content, 1, 1, 1, true)
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(content.total_matches(), 3);
        assert_eq!(content.total_files(), 0);
        assert_eq!(content.matches().len(), 1);
        assert_eq!(content.matches()[0].path(), "src/a.txt");
        assert_eq!(content.matches()[0].line(), 4);
        assert_eq!(content.matches()[0].before()[0].line(), 3);
        assert_eq!(content.matches()[0].after()[0].line(), 5);

        let files = service
            .grep(
                GrepSpec::new(resolved, "needle", Some("src/**/*.txt"), ["target"], 1)
                    .unwrap()
                    .with_options(GrepOutputMode::Count, 1, 0, 0, true)
                    .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(files.total_matches(), 3);
        assert_eq!(files.total_files(), 2);
        assert_eq!(files.files().len(), 1);
        assert_eq!(files.files()[0].path(), "src/deep/b.txt");
        assert_eq!(files.files()[0].count(), 1);
        assert!(files.matches().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn search_reports_non_utf8_paths_instead_of_publishing_lossy_collisions() {
        use std::os::unix::ffi::OsStringExt as _;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(OsString::from_vec(b"bad-\xff".to_vec())),
            "needle",
        )
        .unwrap();
        std::fs::write(root.path().join("good"), "needle").unwrap();
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let output = service
            .glob(
                GlobSpec::new(
                    resolve(&service, root.path(), root.path()),
                    "**/*",
                    ["target"],
                    200,
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output.matches(), &["good"]);
        assert_eq!(output.report().non_utf8, 1);
        assert!(output.report().incomplete());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn searches_report_unrepresentable_paths_and_non_text_lines() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("bad:name"), "needle").unwrap();
        std::fs::write(root.path().join("good"), "needle\u{1b}bad\nneedle fine\n").unwrap();
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let output = service
            .grep(
                GrepSpec::new(
                    resolve(&service, root.path(), root.path()),
                    "needle",
                    None,
                    ["target"],
                    200,
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output.matches().len(), 1);
        assert_eq!(output.matches()[0].line(), 2);
        assert_eq!(output.report().unsupported_paths, 1);
        assert_eq!(output.report().non_text_lines, 1);
        assert!(output.report().incomplete());
    }

    #[tokio::test]
    async fn grep_file_scan_limit_keeps_earlier_matches_and_reports_unsearched_tail() {
        let root = tempfile::tempdir().unwrap();
        let mut file = std::fs::File::create(root.path().join("large.txt")).unwrap();
        file.write_all(b"needle first\n").unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..64 {
            file.write_all(&chunk).unwrap();
        }
        file.write_all(b"\nneedle beyond limit\n").unwrap();
        drop(file);
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let output = service
            .grep(
                GrepSpec::new(
                    resolve(&service, root.path(), root.path()),
                    "needle",
                    None,
                    ["target"],
                    200,
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output.total_matches(), 1);
        assert_eq!(output.matches()[0].text(), "needle first");
        assert_eq!(output.report().limited_files, 1);
        assert!(output.report().incomplete());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_descendant_does_not_hide_readable_sibling_matches() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let denied = root.path().join("denied");
        std::fs::create_dir(&denied).unwrap();
        std::fs::write(denied.join("hidden"), "needle").unwrap();
        std::fs::write(root.path().join("visible"), "needle").unwrap();
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o0)).unwrap();
        // Root can bypass permissions; exercise this contract on ordinary users.
        let permission_enforced = std::fs::read_dir(&denied).is_err();
        let result = service
            .grep(
                GrepSpec::new(
                    resolve(&service, root.path(), root.path()),
                    "needle",
                    None,
                    ["target"],
                    200,
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await;
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = result.unwrap();
        if permission_enforced {
            assert_eq!(output.matches().len(), 1);
            assert_eq!(output.matches()[0].path(), "visible");
            assert_eq!(output.report().unreadable, 1);
            assert!(output.report().incomplete());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn grep_scan_can_be_cancelled_from_the_same_runtime_thread() {
        let root = tempfile::tempdir().unwrap();
        let mut file = std::fs::File::create(root.path().join("large.txt")).unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..65 {
            file.write_all(&chunk).unwrap();
        }
        drop(file);
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let spec = GrepSpec::new(
            resolve(&service, root.path(), root.path()),
            "needle",
            None,
            ["target"],
            200,
        )
        .unwrap();
        let (result, ()) = tokio::join!(service.grep(spec, cancellation), async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            cancel.cancel();
        });
        assert_eq!(result.unwrap_err().code(), FileSystemErrorCode::Cancelled);
    }

    #[tokio::test]
    async fn searches_order_published_paths_before_applying_the_limit() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("a")).unwrap();
        for name in ["a/x", "a-b", "a.rs", "z"] {
            std::fs::write(root.path().join(name), "needle\n").unwrap();
        }
        let service = crate::FileSystemService::local(policy(root.path())).unwrap();
        let resolved = resolve(&service, root.path(), root.path());
        for limit in [1, 2, 4] {
            let glob = service
                .glob(
                    GlobSpec::new(resolved.clone(), "**/*", ["target"], limit).unwrap(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(glob.total_matches(), 4);
            let expected = ["a-b", "a.rs", "a/x", "z"];
            assert_eq!(glob.matches(), &expected[..limit]);
            let grep = service
                .grep(
                    GrepSpec::new(resolved.clone(), "needle", None, ["target"], limit).unwrap(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(grep.total_matches(), 4);
            assert_eq!(
                grep.matches()
                    .iter()
                    .map(GrepMatch::path)
                    .collect::<Vec<_>>(),
                &expected[..limit]
            );
        }
    }

    #[tokio::test]
    async fn wide_glob_does_not_hold_a_handle_for_each_pending_directory() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..512 {
            let directory = root.path().join(format!("crate-{index:03}"));
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("Cargo.toml"), "[package]").unwrap();
        }
        let backend = LocalFileSystemBackend::new(policy(root.path())).unwrap();
        let resolved = backend
            .resolve(PathRequest::new(root.path(), root.path()).unwrap())
            .unwrap();
        let spec = GlobSpec::new(resolved, "*/Cargo.toml", ["target"], 100).unwrap();
        let output = backend.glob(spec, CancellationToken::new()).await.unwrap();
        assert_eq!(output.total_matches(), 512);
        assert_eq!(output.matches().len(), 100);
        assert_eq!(output.matches()[0], "crate-000/Cargo.toml");
    }

    #[test]
    fn matcher_and_binary_probe_preserve_legacy_boundaries() {
        assert!(glob_match("**/*.rs", &["src", "main.rs"]));
        assert!(glob_match("**/*.rs", &["main.rs"]));
        assert!(!glob_match("src/*.rs", &["nested", "main.rs"]));
        assert!(glob_match_one("a?c", "abc"));

        let mut probe = vec![b'a'; 100];
        probe[..10].fill(0);
        assert!(!looks_binary(&probe));
        probe[10] = 0;
        assert!(looks_binary(&probe));
    }

    #[test]
    fn windows_device_and_stream_components_are_refused_lexically() {
        for unsafe_name in [
            ".",
            "..",
            "CON",
            "con.txt",
            "PRN.tar.gz",
            "AUX",
            "CLOCK$",
            "NUL.json",
            "CONIN$",
            "CONOUT$",
            "COM1",
            "com9.log",
            "COM¹.txt",
            "LPT1",
            "lpt9.log",
            "LPT³.txt",
            "file:stream",
            "trailing.",
            "trailing ",
            "bad<name",
            "bad>name",
            "bad\"name",
            "bad|name",
            "bad?name",
            "bad*name",
            "control\u{1f}",
        ] {
            assert!(
                !windows_component_is_safe(OsStr::new(unsafe_name)),
                "Windows-special component was admitted: {unsafe_name:?}"
            );
        }
        for ordinary in [
            "CONSOLE",
            "COM10",
            "LPT10",
            "report.txt",
            ".gitignore",
            "space inside",
            "unicode-文件",
        ] {
            assert!(
                windows_component_is_safe(OsStr::new(ordinary)),
                "ordinary Windows component was over-blocked: {ordinary:?}"
            );
        }
    }

    #[test]
    fn matcher_handles_the_maximum_pattern_shape_without_recursion() {
        let many_stars = "*".repeat(64 * 1024);
        assert!(glob_match_one(&many_stars, "target"));
        let many_segments = vec!["**"; 4_096];
        assert!(match_segments(&many_segments, &["one", "two"]));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parent_symlink_swap_is_rejected_before_commit() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("parent")).unwrap();
        std::fs::write(root.path().join("parent/file.txt"), "original\n").unwrap();
        std::fs::write(outside.path().join("file.txt"), "outside\n").unwrap();
        let swapped = Arc::new(AtomicBool::new(false));
        let swapped_for_hook = Arc::clone(&swapped);
        let root_path = root.path().to_path_buf();
        let outside_path = outside.path().to_path_buf();
        let hook = Arc::new(move |_target: &Path| {
            if !swapped_for_hook.swap(true, Ordering::SeqCst) {
                std::fs::rename(root_path.join("parent"), root_path.join("parked")).unwrap();
                symlink(&outside_path, root_path.join("parent")).unwrap();
            }
        });
        let backend = LocalFileSystemBackend::new_with_hook(policy(root.path()), hook).unwrap();
        let service = crate::FileSystemService::new(Arc::new(backend));
        let path = resolve(&service, root.path(), &root.path().join("parent/file.txt"));
        service
            .read(
                ReadFileSpec::new(path.clone(), 1024).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let error = service
            .write(
                WriteFileSpec::new(path, "changed\n").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), FileSystemErrorCode::ChangedAtCommit);
        assert_eq!(
            std::fs::read_to_string(outside.path().join("file.txt")).unwrap(),
            "outside\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("parked/file.txt")).unwrap(),
            "original\n"
        );
    }

    #[tokio::test]
    async fn a_concurrent_read_cannot_refresh_away_the_prepared_edits_revision_guard() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file.txt");
        std::fs::write(&file, "original\n").unwrap();
        let observed = Arc::new(std::sync::Mutex::new(None::<ObservationLog>));
        let observed_hook = observed.clone();
        let hook = Arc::new(move |target: &Path| {
            std::fs::write(target, "external newer content\n").unwrap();
            observed_hook.lock().unwrap().as_ref().unwrap().mark(target);
        });
        let backend = LocalFileSystemBackend::new_with_hook(policy(root.path()), hook).unwrap();
        *observed.lock().unwrap() = Some(backend.observations.clone());
        let service = crate::FileSystemService::new(Arc::new(backend));
        let path = resolve(&service, root.path(), &file);
        service
            .read(
                ReadFileSpec::new(path.clone(), 1024).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let error = service
            .edit(
                EditFileSpec::new(path, "original", "agent", false).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), FileSystemErrorCode::StaleObservation);
        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            "external newer content\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn capability_root_swap_is_rejected_before_commit() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root_path = parent.path().join("workspace");
        std::fs::create_dir(&root_path).unwrap();
        std::fs::write(root_path.join("file.txt"), "original\n").unwrap();
        std::fs::write(outside.path().join("file.txt"), "outside\n").unwrap();
        let swapped = Arc::new(AtomicBool::new(false));
        let swapped_for_hook = Arc::clone(&swapped);
        let root_for_hook = root_path.clone();
        let parked = parent.path().join("parked");
        let parked_for_hook = parked.clone();
        let outside_path = outside.path().to_path_buf();
        let hook = Arc::new(move |_target: &Path| {
            if !swapped_for_hook.swap(true, Ordering::SeqCst) {
                std::fs::rename(&root_for_hook, &parked_for_hook).unwrap();
                symlink(&outside_path, &root_for_hook).unwrap();
            }
        });
        let backend = LocalFileSystemBackend::new_with_hook(policy(&root_path), hook).unwrap();
        let service = crate::FileSystemService::new(Arc::new(backend));
        let path = resolve(&service, &root_path, &root_path.join("file.txt"));
        service
            .read(
                ReadFileSpec::new(path.clone(), 1024).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let error = service
            .write(
                WriteFileSpec::new(path, "changed\n").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), FileSystemErrorCode::ChangedAtCommit);
        assert_eq!(
            std::fs::read_to_string(outside.path().join("file.txt")).unwrap(),
            "outside\n"
        );
        assert_eq!(
            std::fs::read_to_string(parked.join("file.txt")).unwrap(),
            "original\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn final_symlink_swap_is_rejected_before_commit() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = root.path().join("file.txt");
        let outside_target = outside.path().join("outside.txt");
        std::fs::write(&target, "original\n").unwrap();
        std::fs::write(&outside_target, "outside\n").unwrap();
        let swapped = Arc::new(AtomicBool::new(false));
        let swapped_for_hook = Arc::clone(&swapped);
        let target_for_hook = target.clone();
        let outside_for_hook = outside_target.clone();
        let hook = Arc::new(move |_target: &Path| {
            if !swapped_for_hook.swap(true, Ordering::SeqCst) {
                std::fs::remove_file(&target_for_hook).unwrap();
                symlink(&outside_for_hook, &target_for_hook).unwrap();
            }
        });
        let backend = LocalFileSystemBackend::new_with_hook(policy(root.path()), hook).unwrap();
        let service = crate::FileSystemService::new(Arc::new(backend));
        let path = resolve(&service, root.path(), &target);
        service
            .read(
                ReadFileSpec::new(path.clone(), 1024).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let error = service
            .edit(
                EditFileSpec::new(path, "original", "changed", false).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error.code(),
            FileSystemErrorCode::ChangedAtCommit | FileSystemErrorCode::StaleObservation
        ));
        assert_eq!(
            std::fs::read_to_string(outside_target).unwrap(),
            "outside\n"
        );
    }

    #[tokio::test]
    async fn new_target_created_during_commit_is_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("new.txt");
        let target_for_hook = target.clone();
        let hook = Arc::new(move |_target: &Path| {
            std::fs::write(&target_for_hook, "racer\n").unwrap();
        });
        let backend = LocalFileSystemBackend::new_with_hook(policy(root.path()), hook).unwrap();
        let service = crate::FileSystemService::new(Arc::new(backend));
        let path = resolve(&service, root.path(), &target);
        let error = service
            .write(
                WriteFileSpec::new(path, "agent\n").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), FileSystemErrorCode::ChangedAtCommit);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "racer\n");
        assert!(std::fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".heycode-")
        }));
    }
}
