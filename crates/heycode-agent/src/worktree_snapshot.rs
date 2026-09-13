//! Bounded untracked snapshots through directory capabilities, without following
//! any parent or leaf symlink while reading or writing regular files.

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions, Permissions};
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(PartialEq, Eq)]
pub(crate) enum Contents {
    File(Vec<u8>, Permissions),
    Link(PathBuf),
}
pub(crate) type Snapshot = Vec<(PathBuf, Contents)>;

fn invalid() -> io::Error {
    io::Error::other("unsafe or changing untracked snapshot")
}

fn parent(root: &Dir, path: &Path, create: bool) -> io::Result<Dir> {
    let mut dir = root.try_clone()?;
    for component in path.parent().ok_or_else(invalid)?.components() {
        let Component::Normal(name) = component else {
            return Err(invalid());
        };
        if create {
            match dir.create_dir(name) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error),
            }
        }
        dir = dir.open_dir_nofollow(name)?;
    }
    Ok(dir)
}

pub(crate) fn capture(source: &Path, manifest: &[u8]) -> io::Result<Snapshot> {
    let root = Dir::open_ambient_dir(source, cap_std::ambient_authority())?;
    let mut result = Vec::new();
    let mut total = 0_u64;
    for name in manifest
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let path = PathBuf::from(std::str::from_utf8(name).map_err(|_| invalid())?);
        if path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
            || result.len() >= 10000
        {
            return Err(invalid());
        }
        let dir = parent(&root, &path, false)?;
        let leaf = path.file_name().ok_or_else(invalid)?;
        let metadata = dir.symlink_metadata(leaf)?;
        total = total.checked_add(metadata.len()).ok_or_else(invalid)?;
        if total > LIMIT {
            return Err(invalid());
        }
        let contents = if metadata.is_file() {
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let mut file = dir.open_with(leaf, &options)?;
            let opened = file.metadata()?;
            if !opened.is_file() || opened.len() != metadata.len() {
                return Err(invalid());
            }
            let mut bytes = Vec::new();
            std::io::Read::by_ref(&mut file)
                .take(metadata.len() + 1)
                .read_to_end(&mut bytes)?;
            let after = file.metadata()?;
            if bytes.len() as u64 != metadata.len()
                || after.len() != opened.len()
                || after.modified()? != opened.modified()?
                || after.permissions() != opened.permissions()
            {
                return Err(invalid());
            }
            Contents::File(bytes, opened.permissions())
        } else if metadata.file_type().is_symlink() {
            Contents::Link(dir.read_link_contents(leaf)?)
        } else {
            return Err(invalid());
        };
        result.push((path, contents));
    }
    Ok(result)
}

pub(crate) fn write(destination: &Path, snapshot: &Snapshot) -> io::Result<()> {
    let root = Dir::open_ambient_dir(destination, cap_std::ambient_authority())?;
    for (path, contents) in snapshot {
        let dir = parent(&root, path, true)?;
        let leaf = path.file_name().ok_or_else(invalid)?;
        match contents {
            Contents::File(bytes, permissions) => {
                let mut options = OpenOptions::new();
                options
                    .write(true)
                    .create_new(true)
                    .follow(FollowSymlinks::No);
                let mut file = dir.open_with(leaf, &options)?;
                file.write_all(bytes)?;
                file.set_permissions(permissions.clone())?;
                file.sync_all()?;
            }
            Contents::Link(target) => {
                #[cfg(unix)]
                dir.symlink_contents(target, leaf)?;
                #[cfg(not(unix))]
                {
                    let _ = target;
                    return Err(invalid());
                }
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[test]
    fn capture_is_bounded_and_never_follows_links_or_destination_parents() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let outside = temp.path().join("outside");
        for path in [&source, &destination, &outside] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::create_dir(source.join("nested")).unwrap();
        std::fs::write(source.join("nested/run"), b"payload").unwrap();
        std::fs::set_permissions(
            source.join("nested/run"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        symlink(&outside, source.join("link")).unwrap();
        std::fs::write(outside.join("secret"), b"outside").unwrap();
        assert!(capture(&source, b"link/secret\0").is_err());
        let snapshot = capture(&source, b"nested/run\0link\0").unwrap();
        symlink(&outside, destination.join("nested")).unwrap();
        assert!(write(&destination, &snapshot).is_err());
        assert!(!outside.join("run").exists());
        std::fs::remove_file(destination.join("nested")).unwrap();
        write(&destination, &snapshot).unwrap();
        assert_eq!(
            std::fs::read(destination.join("nested/run")).unwrap(),
            b"payload"
        );
        assert_eq!(
            std::fs::metadata(destination.join("nested/run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::read_link(destination.join("link")).unwrap(),
            outside
        );
        let large = std::fs::File::create(source.join("huge")).unwrap();
        large.set_len(LIMIT + 1).unwrap();
        assert!(capture(&source, b"huge\0").is_err());
    }
}
