//! Bounded, race-detecting admission of a local package directory.

use std::collections::BTreeSet;
use std::path::Path;

use sha2::{Digest as _, Sha256};

use crate::cache_error::io;
use crate::model::valid_relative_path;
use crate::{
    ManifestValidator, PackageCacheError, PackageContentHash, PackageLimit, PackageSourceIssue,
    PluginManifest,
};

pub(crate) const MANIFEST_PATH: &str = ".heycode-plugin/plugin.toml";
const MAX_PACKAGE_ENTRIES: usize = 8_192;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DIRECTORY_DEPTH: usize = 64;

pub(crate) enum PackageEntryData {
    Directory,
    File { bytes: Vec<u8>, executable: bool },
}

pub(crate) struct PackageEntry {
    pub(crate) path: String,
    pub(crate) data: PackageEntryData,
}

pub(crate) struct PreparedPackage {
    pub(crate) manifest: PluginManifest,
    pub(crate) entries: Vec<PackageEntry>,
    pub(crate) content_hash: PackageContentHash,
}

pub(crate) fn read_package_directory(
    source: &Path,
    validator: &ManifestValidator,
) -> Result<PreparedPackage, PackageCacheError> {
    #[cfg(unix)]
    {
        unix::read(source, validator)
    }
    #[cfg(not(unix))]
    {
        let _ = (source, validator);
        Err(PackageCacheError::UnsupportedSecurity)
    }
}

/// Schema v1 hashes domain, entry count, then sorted path length/path/type and
/// exact file executable flag, length, and bytes. Host metadata is excluded.
fn hash_entries(entries: &[PackageEntry]) -> PackageContentHash {
    let mut hasher = Sha256::new();
    hasher.update(b"dshx-plugin-package\0\x01");
    update_length(&mut hasher, entries.len());
    for entry in entries {
        update_length(&mut hasher, entry.path.len());
        hasher.update(entry.path.as_bytes());
        match &entry.data {
            PackageEntryData::Directory => hasher.update([0]),
            PackageEntryData::File { bytes, executable } => {
                hasher.update([1, u8::from(*executable)]);
                update_length(&mut hasher, bytes.len());
                hasher.update(bytes);
            }
        }
    }
    PackageContentHash::from_digest(hasher.finalize().into())
}

fn update_length(hasher: &mut Sha256, value: usize) {
    hasher.update(u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes());
}

#[cfg(unix)]
mod unix {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;

    use cap_std::ambient_authority;
    use cap_std::fs::{Dir, DirEntry, Metadata, MetadataExt as _};

    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct DirectoryStamp {
        device: u64,
        inode: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        changed_seconds: i64,
        changed_nanoseconds: i64,
    }

    impl DirectoryStamp {
        fn from_metadata(metadata: &Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct FileStamp {
        device: u64,
        inode: u64,
        length: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        changed_seconds: i64,
        changed_nanoseconds: i64,
        links: u64,
        executable: bool,
    }

    impl FileStamp {
        fn from_metadata(metadata: &Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
                links: metadata.nlink(),
                executable: metadata.mode() & 0o111 != 0,
            }
        }
    }

    struct WalkState {
        entries: Vec<PackageEntry>,
        portable_paths: BTreeSet<String>,
        total_bytes: u64,
    }

    pub(super) fn read(
        source: &Path,
        validator: &ManifestValidator,
    ) -> Result<PreparedPackage, PackageCacheError> {
        let ambient_metadata =
            std::fs::symlink_metadata(source).map_err(|_| io("inspect_source"))?;
        if ambient_metadata.file_type().is_symlink() {
            return unsafe_source(PackageSourceIssue::SymbolicLink);
        }
        if !ambient_metadata.is_dir() {
            return unsafe_source(PackageSourceIssue::UnsupportedFileType);
        }
        let source_directory = Dir::open_ambient_dir(source, ambient_authority())
            .map_err(|_| io("open_source_directory"))?;
        let opened_metadata = source_directory
            .dir_metadata()
            .map_err(|_| io("inspect_source"))?;
        if ambient_metadata.dev() != opened_metadata.dev()
            || ambient_metadata.ino() != opened_metadata.ino()
        {
            return unsafe_source(PackageSourceIssue::ChangedDuringRead);
        }
        read_opened(&source_directory, validator)
    }

    fn read_opened(
        source_directory: &Dir,
        validator: &ManifestValidator,
    ) -> Result<PreparedPackage, PackageCacheError> {
        let mut state = WalkState {
            entries: Vec::new(),
            portable_paths: BTreeSet::new(),
            total_bytes: 0,
        };
        walk_directory(source_directory, "", 0, &mut state)?;
        state
            .entries
            .sort_by(|left, right| left.path.cmp(&right.path));

        let manifest_bytes = state
            .entries
            .iter()
            .find_map(|entry| {
                if entry.path == MANIFEST_PATH {
                    match &entry.data {
                        PackageEntryData::File { bytes, .. } => Some(bytes.as_slice()),
                        PackageEntryData::Directory => None,
                    }
                } else {
                    None
                }
            })
            .ok_or(PackageCacheError::MissingManifest)?;
        let manifest_raw =
            std::str::from_utf8(manifest_bytes).map_err(|_| PackageCacheError::ManifestNotUtf8)?;
        let manifest = validator.validate_toml(manifest_raw)?;
        let paths = state
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<BTreeSet<_>>();
        if manifest
            .contributions()
            .iter()
            .any(|contribution| !paths.contains(contribution.path().as_str()))
            || manifest
                .configuration_schema()
                .is_some_and(|schema| !paths.contains(schema.as_str()))
        {
            return Err(PackageCacheError::MissingDeclaredPath);
        }
        let content_hash = hash_entries(&state.entries);
        Ok(PreparedPackage {
            manifest,
            entries: state.entries,
            content_hash,
        })
    }

    fn walk_directory(
        directory: &Dir,
        relative: &str,
        depth: usize,
        state: &mut WalkState,
    ) -> Result<(), PackageCacheError> {
        if depth > MAX_DIRECTORY_DEPTH {
            return Err(PackageCacheError::PackageLimit {
                limit: PackageLimit::Depth,
            });
        }
        let before = directory_metadata(directory)?;
        let mut children = directory
            .entries()
            .map_err(|_| io("read_source_directory"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| io("read_source_directory"))?;
        children.sort_by_key(DirEntry::file_name);
        for child in children {
            let name =
                child
                    .file_name()
                    .into_string()
                    .map_err(|_| PackageCacheError::UnsafeSource {
                        issue: PackageSourceIssue::NonPortablePath,
                    })?;
            let child_relative = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            if !valid_relative_path(&child_relative) {
                return unsafe_source(PackageSourceIssue::NonPortablePath);
            }
            if !state.portable_paths.insert(child_relative.to_lowercase()) {
                return unsafe_source(PackageSourceIssue::CaseCollision);
            }
            if state.entries.len() >= MAX_PACKAGE_ENTRIES {
                return Err(PackageCacheError::PackageLimit {
                    limit: PackageLimit::Entries,
                });
            }
            let file_type = child.file_type().map_err(|_| io("inspect_source"))?;
            if file_type.is_symlink() {
                return unsafe_source(PackageSourceIssue::SymbolicLink);
            }
            if file_type.is_dir() {
                let before_open = child.metadata().map_err(|_| io("inspect_source"))?;
                let child_directory =
                    child
                        .open_dir()
                        .map_err(|_| PackageCacheError::UnsafeSource {
                            issue: PackageSourceIssue::ChangedDuringRead,
                        })?;
                let after_open = child_directory
                    .dir_metadata()
                    .map_err(|_| io("inspect_source"))?;
                if before_open.dev() != after_open.dev() || before_open.ino() != after_open.ino() {
                    return unsafe_source(PackageSourceIssue::ChangedDuringRead);
                }
                state.entries.push(PackageEntry {
                    path: child_relative.clone(),
                    data: PackageEntryData::Directory,
                });
                walk_directory(&child_directory, &child_relative, depth + 1, state)?;
            } else if file_type.is_file() {
                let (bytes, executable) = read_file(&child, state)?;
                state.entries.push(PackageEntry {
                    path: child_relative,
                    data: PackageEntryData::File { bytes, executable },
                });
            } else {
                return unsafe_source(PackageSourceIssue::UnsupportedFileType);
            }
        }
        let after = directory_metadata(directory)?;
        if before != after {
            return unsafe_source(PackageSourceIssue::ChangedDuringRead);
        }
        Ok(())
    }

    fn directory_metadata(directory: &Dir) -> Result<DirectoryStamp, PackageCacheError> {
        let metadata = directory.dir_metadata().map_err(|_| io("inspect_source"))?;
        if !metadata.is_dir() {
            return unsafe_source(PackageSourceIssue::ChangedDuringRead);
        }
        Ok(DirectoryStamp::from_metadata(&metadata))
    }

    fn read_file(
        entry: &DirEntry,
        state: &mut WalkState,
    ) -> Result<(Vec<u8>, bool), PackageCacheError> {
        let before =
            FileStamp::from_metadata(&entry.metadata().map_err(|_| io("inspect_source_file"))?);
        validate_file_stamp(before)?;
        if before.length > MAX_FILE_BYTES {
            return Err(PackageCacheError::PackageLimit {
                limit: PackageLimit::FileBytes,
            });
        }
        if state
            .total_bytes
            .checked_add(before.length)
            .is_none_or(|total| total > MAX_TOTAL_BYTES)
        {
            return Err(PackageCacheError::PackageLimit {
                limit: PackageLimit::TotalBytes,
            });
        }
        let mut file = entry.open().map_err(|_| PackageCacheError::UnsafeSource {
            issue: PackageSourceIssue::ChangedDuringRead,
        })?;
        let opened =
            FileStamp::from_metadata(&file.metadata().map_err(|_| io("inspect_source_file"))?);
        validate_file_stamp(opened)?;
        if opened != before {
            return unsafe_source(PackageSourceIssue::ChangedDuringRead);
        }
        let mut bytes = Vec::with_capacity(usize::try_from(before.length).unwrap_or(0));
        file.by_ref()
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| io("read_source_file"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
            return Err(PackageCacheError::PackageLimit {
                limit: PackageLimit::FileBytes,
            });
        }
        let after_open =
            FileStamp::from_metadata(&file.metadata().map_err(|_| io("inspect_source_file"))?);
        let after_path =
            FileStamp::from_metadata(&entry.metadata().map_err(|_| io("inspect_source_file"))?);
        validate_file_stamp(after_open)?;
        validate_file_stamp(after_path)?;
        if before != after_open || before != after_path || before.length != bytes.len() as u64 {
            return unsafe_source(PackageSourceIssue::ChangedDuringRead);
        }
        state.total_bytes += before.length;
        Ok((bytes, before.executable))
    }

    fn validate_file_stamp(stamp: FileStamp) -> Result<(), PackageCacheError> {
        if stamp.links != 1 {
            return unsafe_source(PackageSourceIssue::HardLink);
        }
        Ok(())
    }

    fn unsafe_source<T>(issue: PackageSourceIssue) -> Result<T, PackageCacheError> {
        Err(PackageCacheError::UnsafeSource { issue })
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)]

        use std::os::unix::fs::symlink;

        use crate::{ApiVersion, Architecture, OperatingSystem, PlatformTarget};

        use super::*;

        #[test]
        fn held_source_capability_ignores_ambient_root_symlink_replacement() {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            std::fs::create_dir_all(source.join(".heycode-plugin")).unwrap();
            std::fs::create_dir_all(source.join("skills/review")).unwrap();
            let manifest = r#"schema_version = 1
id = "local/capability"
name = "Capability fixture"
version = "1.0.0"
description = "Held directory capability fixture."
license = "MIT"
default_enabled = false
requested_permissions = []
platforms = [{ os = "macos", architecture = "aarch64" }]
dependencies = []
conflicts = []
contributions = [{ kind = "skill", id = "review", path = "skills/review/SKILL.md", exposure = { mode = "namespaced" } }]

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixtures/capability"
revision = "v1"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#;
            std::fs::write(source.join(MANIFEST_PATH), manifest).unwrap();
            std::fs::write(source.join("skills/review/SKILL.md"), b"original\n").unwrap();
            let held = Dir::open_ambient_dir(&source, ambient_authority()).unwrap();

            let moved = temp.path().join("moved");
            std::fs::rename(&source, &moved).unwrap();
            let outside = temp.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            symlink(&outside, &source).unwrap();

            let validator = ManifestValidator::new(
                ApiVersion::new(1).unwrap(),
                PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64),
            );
            let package = read_opened(&held, &validator).unwrap();
            assert_eq!(package.manifest.id().as_str(), "local/capability");
            assert!(package.entries.iter().any(|entry| {
                entry.path == "skills/review/SKILL.md"
                    && matches!(
                        &entry.data,
                        PackageEntryData::File { bytes, .. } if bytes == b"original\n"
                    )
            }));
        }
    }
}
