//! Audited local immutable attachment backend.

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use cap_std::ambient_authority;
    use cap_std::fs::{
        Dir, Metadata, MetadataExt as _, OpenOptions, OpenOptionsExt as _, Permissions,
    };
    use tokio_util::sync::CancellationToken;

    use crate::{AttachmentBackend, AttachmentStoreError};

    const SCHEMA_FILE: &str = "schema-v1";
    const SCHEMA_BYTES: &[u8] = b"dshx-attachments\n1\n";
    const LOCK_FILE: &str = ".write.lock";
    const READ_CHUNK_BYTES: usize = 64 * 1024;

    static PROCESS_GATE: OnceLock<Mutex<()>> = OnceLock::new();

    pub(crate) struct LocalAttachmentBackend {
        root: Dir,
        lock: std::fs::File,
    }

    impl LocalAttachmentBackend {
        pub(crate) fn open(root: &Path) -> Result<Self, AttachmentStoreError> {
            let canonical = open_root(root)?;
            let lock = open_lock_file(&canonical.join(LOCK_FILE))?;
            let backend = Self {
                root: Dir::open_ambient_dir(&canonical, ambient_authority())
                    .map_err(|_| AttachmentStoreError::io())?,
                lock,
            };
            backend.with_lock(|| {
                ensure_directory(&backend.root, Path::new("objects"))?;
                ensure_directory(&backend.root, Path::new("objects/sha256"))?;
                ensure_schema(&backend.root)
            })?;
            Ok(backend)
        }

        fn with_lock<T>(
            &self,
            operation: impl FnOnce() -> Result<T, AttachmentStoreError>,
        ) -> Result<T, AttachmentStoreError> {
            let process = PROCESS_GATE
                .get_or_init(|| Mutex::new(()))
                .lock()
                .map_err(|_| AttachmentStoreError::unavailable())?;
            let lock = FlockGuard::acquire(&self.lock, process)?;
            let result = operation();
            drop(lock);
            result
        }

        fn object_parent(
            &self,
            content_id: &heycode_core::AttachmentContentId,
            create: bool,
        ) -> Result<(Dir, OsString), AttachmentStoreError> {
            let digest = content_id.digest_hex();
            let prefix = &digest[..2];
            let directory = PathBuf::from("objects/sha256").join(prefix);
            if create {
                ensure_directory(&self.root, &directory)?;
            }
            let parent = self
                .root
                .open_dir(&directory)
                .map_err(|_| AttachmentStoreError::corrupt())?;
            validate_directory_metadata(
                &parent
                    .dir_metadata()
                    .map_err(|_| AttachmentStoreError::corrupt())?,
            )?;
            Ok((parent, OsString::from(&digest[2..])))
        }
    }

    impl AttachmentBackend for LocalAttachmentBackend {
        fn put(
            &self,
            content_id: &heycode_core::AttachmentContentId,
            bytes: &[u8],
            caller_cancellation: &CancellationToken,
            lifecycle_cancellation: &CancellationToken,
        ) -> Result<(), AttachmentStoreError> {
            self.with_lock(|| {
                check(caller_cancellation, lifecycle_cancellation)?;
                let (parent, final_name) = self.object_parent(content_id, true)?;
                match parent.symlink_metadata(&final_name) {
                    Ok(_) => {
                        let existing = read_file(
                            &parent,
                            &final_name,
                            bytes.len(),
                            caller_cancellation,
                            lifecycle_cancellation,
                        )?;
                        if existing == bytes {
                            return Ok(());
                        }
                        return Err(AttachmentStoreError::corrupt());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(AttachmentStoreError::io()),
                }

                let temporary_name = OsString::from(format!(".tmp-{}", uuid::Uuid::new_v4()));
                let mut options = OpenOptions::new();
                options.write(true).create_new(true).mode(0o600);
                let mut temporary = parent
                    .open_with(&temporary_name, &options)
                    .map_err(|_| AttachmentStoreError::io())?;
                let mut guard = TemporaryEntry {
                    parent: &parent,
                    name: &temporary_name,
                    active: true,
                };
                for chunk in bytes.chunks(READ_CHUNK_BYTES) {
                    check(caller_cancellation, lifecycle_cancellation)?;
                    temporary
                        .write_all(chunk)
                        .map_err(|_| AttachmentStoreError::io())?;
                }
                temporary
                    .sync_all()
                    .map_err(|_| AttachmentStoreError::io())?;
                validate_file_metadata(
                    &temporary
                        .metadata()
                        .map_err(|_| AttachmentStoreError::io())?,
                )?;
                drop(temporary);
                check(caller_cancellation, lifecycle_cancellation)?;
                match parent.hard_link(&temporary_name, &parent, &final_name) {
                    Ok(()) => {
                        parent
                            .remove_file(&temporary_name)
                            .map_err(|_| AttachmentStoreError::io())?;
                        guard.active = false;
                        sync_directory(&parent)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let existing = read_file(
                            &parent,
                            &final_name,
                            bytes.len(),
                            caller_cancellation,
                            lifecycle_cancellation,
                        )?;
                        if existing != bytes {
                            return Err(AttachmentStoreError::corrupt());
                        }
                        return Ok(());
                    }
                    Err(_) => return Err(AttachmentStoreError::io()),
                }
                let committed = read_file(
                    &parent,
                    &final_name,
                    bytes.len(),
                    caller_cancellation,
                    lifecycle_cancellation,
                )?;
                if committed != bytes {
                    return Err(AttachmentStoreError::corrupt());
                }
                Ok(())
            })
        }

        fn read(
            &self,
            content_id: &heycode_core::AttachmentContentId,
            maximum: usize,
            caller_cancellation: &CancellationToken,
            lifecycle_cancellation: &CancellationToken,
        ) -> Result<Vec<u8>, AttachmentStoreError> {
            self.with_lock(|| {
                check(caller_cancellation, lifecycle_cancellation)?;
                let (parent, name) = self.object_parent(content_id, false)?;
                read_file(
                    &parent,
                    &name,
                    maximum,
                    caller_cancellation,
                    lifecycle_cancellation,
                )
            })
        }
    }

    struct FlockGuard<'a> {
        file: &'a std::fs::File,
        _process: MutexGuard<'a, ()>,
    }

    impl<'a> FlockGuard<'a> {
        #[allow(deprecated)]
        fn acquire(
            file: &'a std::fs::File,
            process: MutexGuard<'a, ()>,
        ) -> Result<Self, AttachmentStoreError> {
            nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusive)
                .map_err(|_| AttachmentStoreError::io())?;
            Ok(Self {
                file,
                _process: process,
            })
        }
    }

    impl Drop for FlockGuard<'_> {
        #[allow(deprecated)]
        fn drop(&mut self) {
            let _ = nix::fcntl::flock(self.file.as_raw_fd(), nix::fcntl::FlockArg::Unlock);
        }
    }

    struct TemporaryEntry<'a> {
        parent: &'a Dir,
        name: &'a std::ffi::OsStr,
        active: bool,
    }

    impl Drop for TemporaryEntry<'_> {
        fn drop(&mut self) {
            if self.active {
                let _ = self.parent.remove_file(self.name);
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct FileIdentity {
        device: u64,
        inode: u64,
        length: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        changed_seconds: i64,
        changed_nanoseconds: i64,
        links: u64,
        mode: u32,
    }

    impl FileIdentity {
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
                mode: metadata.mode() & 0o777,
            }
        }
    }

    fn read_file(
        parent: &Dir,
        name: &std::ffi::OsStr,
        maximum: usize,
        caller_cancellation: &CancellationToken,
        lifecycle_cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, AttachmentStoreError> {
        check(caller_cancellation, lifecycle_cancellation)?;
        let before_metadata = parent
            .symlink_metadata(name)
            .map_err(|_| AttachmentStoreError::corrupt())?;
        validate_file_metadata(&before_metadata)?;
        let before = FileIdentity::from_metadata(&before_metadata);
        if before.length > u64::try_from(maximum).unwrap_or(u64::MAX) {
            return Err(AttachmentStoreError::corrupt());
        }
        let mut file = parent
            .open(name)
            .map_err(|_| AttachmentStoreError::corrupt())?;
        let opened = file
            .metadata()
            .map_err(|_| AttachmentStoreError::corrupt())?;
        validate_file_metadata(&opened)?;
        if FileIdentity::from_metadata(&opened) != before {
            return Err(AttachmentStoreError::corrupt());
        }
        let mut bytes = Vec::with_capacity(usize::try_from(before.length).unwrap_or(0));
        let mut buffer = [0_u8; READ_CHUNK_BYTES];
        loop {
            check(caller_cancellation, lifecycle_cancellation)?;
            let read = file
                .read(&mut buffer)
                .map_err(|_| AttachmentStoreError::io())?;
            if read == 0 {
                break;
            }
            if bytes
                .len()
                .checked_add(read)
                .is_none_or(|length| length > maximum)
            {
                return Err(AttachmentStoreError::corrupt());
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        let after_open = file
            .metadata()
            .map_err(|_| AttachmentStoreError::corrupt())?;
        let after_path = parent
            .symlink_metadata(name)
            .map_err(|_| AttachmentStoreError::corrupt())?;
        validate_file_metadata(&after_open)?;
        validate_file_metadata(&after_path)?;
        if FileIdentity::from_metadata(&after_open) != before
            || FileIdentity::from_metadata(&after_path) != before
            || u64::try_from(bytes.len()).ok() != Some(before.length)
        {
            return Err(AttachmentStoreError::corrupt());
        }
        Ok(bytes)
    }

    fn open_root(root: &Path) -> Result<PathBuf, AttachmentStoreError> {
        if !root.is_absolute() {
            return Err(AttachmentStoreError::invalid_input());
        }
        match std::fs::symlink_metadata(root) {
            Ok(metadata) => validate_std_directory_metadata(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = root
                    .parent()
                    .ok_or_else(AttachmentStoreError::invalid_input)?;
                let parent_metadata = std::fs::symlink_metadata(parent)
                    .map_err(|_| AttachmentStoreError::invalid_input())?;
                if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
                    return Err(AttachmentStoreError::invalid_input());
                }
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700);
                builder
                    .create(root)
                    .map_err(|_| AttachmentStoreError::io())?;
                std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| AttachmentStoreError::io())?;
                validate_std_directory_metadata(
                    &std::fs::symlink_metadata(root).map_err(|_| AttachmentStoreError::io())?,
                )?;
                sync_std_directory(parent)?;
            }
            Err(_) => return Err(AttachmentStoreError::io()),
        }
        let canonical = std::fs::canonicalize(root).map_err(|_| AttachmentStoreError::io())?;
        validate_std_directory_metadata(
            &std::fs::symlink_metadata(&canonical).map_err(|_| AttachmentStoreError::io())?,
        )?;
        Ok(canonical)
    }

    fn ensure_directory(root: &Dir, path: &Path) -> Result<(), AttachmentStoreError> {
        match root.symlink_metadata(path) {
            Ok(metadata) => validate_directory_metadata(&metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                root.create_dir(path)
                    .map_err(|_| AttachmentStoreError::io())?;
                root.set_permissions(
                    path,
                    Permissions::from_std(std::fs::Permissions::from_mode(0o700)),
                )
                .map_err(|_| AttachmentStoreError::io())?;
                let metadata = root
                    .symlink_metadata(path)
                    .map_err(|_| AttachmentStoreError::io())?;
                validate_directory_metadata(&metadata)?;
                sync_directory(root)
            }
            Err(_) => Err(AttachmentStoreError::io()),
        }
    }

    fn ensure_schema(root: &Dir) -> Result<(), AttachmentStoreError> {
        match root.symlink_metadata(SCHEMA_FILE) {
            Ok(_) => {
                let bytes = read_file(
                    root,
                    std::ffi::OsStr::new(SCHEMA_FILE),
                    SCHEMA_BYTES.len(),
                    &CancellationToken::new(),
                    &CancellationToken::new(),
                )?;
                if bytes != SCHEMA_BYTES {
                    return Err(AttachmentStoreError::corrupt());
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true).mode(0o600);
                let mut file = root
                    .open_with(SCHEMA_FILE, &options)
                    .map_err(|_| AttachmentStoreError::io())?;
                file.write_all(SCHEMA_BYTES)
                    .map_err(|_| AttachmentStoreError::io())?;
                file.sync_all().map_err(|_| AttachmentStoreError::io())?;
                validate_file_metadata(&file.metadata().map_err(|_| AttachmentStoreError::io())?)?;
                drop(file);
                sync_directory(root)
            }
            Err(_) => Err(AttachmentStoreError::io()),
        }
    }

    fn open_lock_file(path: &Path) -> Result<std::fs::File, AttachmentStoreError> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true).mode(0o600);
        match options.open(path) {
            Ok(file) => {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| AttachmentStoreError::io())?;
                file.sync_all().map_err(|_| AttachmentStoreError::io())?;
                validate_std_file_metadata(
                    &file.metadata().map_err(|_| AttachmentStoreError::io())?,
                )?;
                if let Some(parent) = path.parent() {
                    sync_std_directory(parent)?;
                }
                Ok(file)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let before =
                    std::fs::symlink_metadata(path).map_err(|_| AttachmentStoreError::corrupt())?;
                validate_std_file_metadata(&before)?;
                let mut existing = std::fs::OpenOptions::new();
                existing.read(true).write(true);
                let file = existing
                    .open(path)
                    .map_err(|_| AttachmentStoreError::corrupt())?;
                let opened = file
                    .metadata()
                    .map_err(|_| AttachmentStoreError::corrupt())?;
                validate_std_file_metadata(&opened)?;
                if before.dev() != opened.dev() || before.ino() != opened.ino() {
                    return Err(AttachmentStoreError::corrupt());
                }
                Ok(file)
            }
            Err(_) => Err(AttachmentStoreError::io()),
        }
    }

    fn validate_directory_metadata(metadata: &Metadata) -> Result<(), AttachmentStoreError> {
        if !metadata.is_dir() || metadata.mode() & 0o777 != 0o700 {
            return Err(AttachmentStoreError::corrupt());
        }
        Ok(())
    }

    fn validate_file_metadata(metadata: &Metadata) -> Result<(), AttachmentStoreError> {
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o777 != 0o600 {
            return Err(AttachmentStoreError::corrupt());
        }
        Ok(())
    }

    fn validate_std_directory_metadata(
        metadata: &std::fs::Metadata,
    ) -> Result<(), AttachmentStoreError> {
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(AttachmentStoreError::corrupt());
        }
        Ok(())
    }

    fn validate_std_file_metadata(
        metadata: &std::fs::Metadata,
    ) -> Result<(), AttachmentStoreError> {
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(AttachmentStoreError::corrupt());
        }
        Ok(())
    }

    fn check(
        caller_cancellation: &CancellationToken,
        lifecycle_cancellation: &CancellationToken,
    ) -> Result<(), AttachmentStoreError> {
        if caller_cancellation.is_cancelled() || lifecycle_cancellation.is_cancelled() {
            Err(AttachmentStoreError::cancelled())
        } else {
            Ok(())
        }
    }

    fn sync_directory(directory: &Dir) -> Result<(), AttachmentStoreError> {
        directory
            .try_clone()
            .map(Dir::into_std_file)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| AttachmentStoreError::io())
    }

    fn sync_std_directory(path: &Path) -> Result<(), AttachmentStoreError> {
        std::fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| AttachmentStoreError::io())
    }
}

#[cfg(not(unix))]
mod unsupported {
    use std::path::Path;

    use tokio_util::sync::CancellationToken;

    use crate::{AttachmentBackend, AttachmentStoreError};

    pub(crate) struct LocalAttachmentBackend;

    impl LocalAttachmentBackend {
        pub(crate) fn open(_root: &Path) -> Result<Self, AttachmentStoreError> {
            Err(AttachmentStoreError::unsupported_security())
        }
    }

    impl AttachmentBackend for LocalAttachmentBackend {
        fn put(
            &self,
            _content_id: &heycode_core::AttachmentContentId,
            _bytes: &[u8],
            _caller_cancellation: &CancellationToken,
            _lifecycle_cancellation: &CancellationToken,
        ) -> Result<(), AttachmentStoreError> {
            Err(AttachmentStoreError::unsupported_security())
        }

        fn read(
            &self,
            _content_id: &heycode_core::AttachmentContentId,
            _maximum: usize,
            _caller_cancellation: &CancellationToken,
            _lifecycle_cancellation: &CancellationToken,
        ) -> Result<Vec<u8>, AttachmentStoreError> {
            Err(AttachmentStoreError::unsupported_security())
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::LocalAttachmentBackend;
#[cfg(not(unix))]
pub(crate) use unsupported::LocalAttachmentBackend;
