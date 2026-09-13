//! Durable native edit checkpoints. Only confirmed tool edits are restorable.
//! Conversation rewind uses an ordinary verified session fork, never log truncation.
use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

const MAX_FILE: u64 = 2 * 1024 * 1024;
const MAX_RECORDS: usize = 2048;
const MAX_RECORD_BYTES: u64 = 32 * 1024 * 1024;
const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;

/// Store bound to a session directory and an exact workspace directory handle.
pub struct Checkpoints {
    store: Dir,
    workspace: Dir,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    version: u8,
    seq: u64,
    path: PathBuf,
    before: Option<String>,
    after: String,
}

/// One durable preimage, awaiting successful tool settlement.
pub struct PendingEdit {
    name: String,
    edit: Edit,
}

fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}

fn relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid(
            "checkpoint path must be relative without traversal",
        ));
    }
    Ok(())
}

fn parent(root: &Dir, path: &Path) -> io::Result<(Dir, std::ffi::OsString)> {
    relative(path)?;
    let mut dir = root.try_clone()?;
    let mut components = path.components().peekable();
    while let Some(Component::Normal(component)) = components.next() {
        if components.peek().is_none() {
            return Ok((dir, component.to_owned()));
        }
        dir = dir.open_dir_nofollow(component)?;
    }
    Err(invalid("empty checkpoint path"))
}

fn read(root: &Dir, path: &Path) -> io::Result<Option<String>> {
    read_bounded(root, path, MAX_FILE)
}

fn read_bounded(root: &Dir, path: &Path, max: u64) -> io::Result<Option<String>> {
    let (dir, name) = match parent(root, path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = match dir.open_with(name, &options) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    if !file.metadata()?.is_file() {
        return Err(invalid("checkpoint requires a regular file"));
    }
    let mut text = String::new();
    file.take(max + 1).read_to_string(&mut text)?;
    if text.len() as u64 > max {
        return Err(invalid("checkpoint exceeds its size limit"));
    }
    Ok(Some(text))
}

impl Checkpoints {
    /// Count distinct confirmed native file checkpoints at each boundary
    /// without creating a store or changing a workspace file. Counts identify
    /// owned restore candidates; the restore operation still checks current
    /// postimages and conflicts immediately before applying them.
    ///
    /// # Errors
    /// Unsafe, corrupt, oversized or uncertain checkpoint storage.
    pub fn restore_candidate_counts(
        session_dir: &Path,
        workspace: &Path,
        boundaries: &[u64],
    ) -> io::Result<Vec<usize>> {
        let session = Dir::open_ambient_dir(session_dir, cap_std::ambient_authority())?;
        let store = match session.open_dir_nofollow("checkpoints") {
            Ok(store) => store,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(vec![0; boundaries.len()]);
            }
            Err(error) => return Err(error),
        };
        let owner = Self {
            store,
            workspace: Dir::open_ambient_dir(workspace, cap_std::ambient_authority())?,
        };
        let edits = owner.records(boundaries.iter().copied().min())?;
        Ok(boundaries
            .iter()
            .map(|boundary| {
                edits
                    .iter()
                    .filter(|(_, edit)| edit.seq >= *boundary)
                    .map(|(_, edit)| &edit.path)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            })
            .collect())
    }

    /// Open a session-owned store; symlinks inside the store/workspace are refused.
    /// # Errors
    /// Invalid directories or filesystem access failures.
    pub fn open(session_dir: &Path, workspace: &Path) -> io::Result<Self> {
        let session = Dir::open_ambient_dir(session_dir, cap_std::ambient_authority())?;
        match session.create_dir("checkpoints") {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        Ok(Self {
            store: session.open_dir_nofollow("checkpoints")?,
            workspace: Dir::open_ambient_dir(workspace, cap_std::ambient_authority())?,
        })
    }

    /// Read one bounded workspace preimage without following symlinks.
    /// # Errors
    /// Unsafe paths, non-text input, size limits or filesystem failures.
    pub fn read_file(&self, path: &Path) -> io::Result<Option<String>> {
        read(&self.workspace, path)
    }

    /// Persist an exact before/expected-after pair before dispatching a native edit.
    /// # Errors
    /// Unsafe paths, oversized/non-text files, or unavailable durable storage.
    pub fn prepare(&self, seq: u64, path: &Path, after: String) -> io::Result<PendingEdit> {
        relative(path)?;
        if after.len() as u64 > MAX_FILE {
            return Err(invalid("checkpoint file exceeds 2 MiB"));
        }
        let mut count = 0;
        let mut bytes = 0_u64;
        for entry in self.store.entries()? {
            let entry = entry?;
            count += 1;
            bytes = bytes.saturating_add(entry.metadata()?.len());
            if count >= MAX_RECORDS * 2 || bytes > MAX_STORE_BYTES {
                return Err(invalid(
                    "checkpoint retention limit reached; start a new conversation or archive these checkpoints",
                ));
            }
        }
        let edit = Edit {
            version: 1,
            seq,
            path: path.to_owned(),
            before: read(&self.workspace, path)?,
            after,
        };
        let name = format!("{}", uuid::Uuid::new_v4());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self.store.open_with(format!("{name}.json"), &options)?;
        let encoded = serde_json::to_vec(&edit)?;
        if bytes.saturating_add(encoded.len() as u64) > MAX_STORE_BYTES {
            return Err(invalid("checkpoint storage exceeds 64 MiB"));
        }
        file.write_all(&encoded)?;
        file.sync_all()?;
        Ok(PendingEdit { name, edit })
    }

    /// Confirm only a successful tool whose exact postimage is present.
    /// # Errors
    /// Concurrent changes or storage failure; an unconfirmed edit cannot restore.
    pub fn confirm(&self, pending: PendingEdit) -> io::Result<()> {
        if read(&self.workspace, &pending.edit.path)?.as_ref() != Some(&pending.edit.after) {
            return Err(invalid("file changed before checkpoint confirmation"));
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let encoded = serde_json::to_vec(&pending.edit)?;
        let mut confirmation = self
            .store
            .open_with(format!("{}.ok", pending.name), &options)?;
        confirmation.write_all(format!("{:x}", Sha256::digest(encoded)).as_bytes())?;
        confirmation.sync_all()
    }

    fn records(&self, check_unconfirmed_from: Option<u64>) -> io::Result<Vec<(String, Edit)>> {
        let mut edits = Vec::new();
        let mut bytes = 0_u64;
        for (count, entry) in self.store.entries()?.enumerate() {
            if count >= MAX_RECORDS * 2 {
                return Err(invalid("too many checkpoint records"));
            }
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = name.strip_suffix(".ok") {
                if !self.store.try_exists(format!("{id}.json"))? {
                    return Err(invalid("confirmed checkpoint is missing its preimage"));
                }
                continue;
            }
            let Some(id) = name.strip_suffix(".json") else {
                return Err(invalid("unexpected checkpoint store entry"));
            };
            let parsed =
                uuid::Uuid::parse_str(id).map_err(|_| invalid("invalid checkpoint identity"))?;
            if parsed.to_string() != id {
                return Err(invalid("noncanonical checkpoint identity"));
            }
            let raw = read_bounded(&self.store, Path::new(name.as_ref()), MAX_RECORD_BYTES)?
                .ok_or_else(|| invalid("missing checkpoint"))?;
            bytes = bytes.saturating_add(raw.len() as u64);
            if bytes > MAX_STORE_BYTES {
                return Err(invalid("checkpoint restore exceeds 64 MiB"));
            }
            let edit: Edit = serde_json::from_str(&raw)?;
            relative(&edit.path)?;
            if edit.version != 1
                || edit.after.len() as u64 > MAX_FILE
                || edit
                    .before
                    .as_ref()
                    .is_some_and(|s| s.len() as u64 > MAX_FILE)
            {
                return Err(invalid("invalid checkpoint version or size"));
            }
            let confirmation = read_bounded(&self.store, Path::new(&format!("{id}.ok")), 64)?;
            let Some(expected_hash) = confirmation else {
                if check_unconfirmed_from.is_some_and(|boundary| edit.seq >= boundary)
                    && read(&self.workspace, &edit.path)? != edit.before
                {
                    return Err(invalid(&format!(
                        "unconfirmed edit at {}: outcome may be interrupted; no files restored",
                        edit.path.display()
                    )));
                }
                continue;
            };
            if expected_hash != format!("{:x}", Sha256::digest(raw.as_bytes())) {
                return Err(invalid("checkpoint integrity check failed"));
            }
            edits.push((id.to_owned(), edit));
        }
        Ok(edits)
    }

    /// Copy verified earlier checkpoints into a newly forked conversation so
    /// a later rewind can still reach edits in its inherited prefix.
    /// # Errors
    /// Invalid records or destination storage failure. No workspace file changes.
    pub fn copy_prefix_to(&self, session_dir: &Path, boundary: u64) -> io::Result<()> {
        let session = Dir::open_ambient_dir(session_dir, cap_std::ambient_authority())?;
        session.create_dir("checkpoints")?;
        let target = session.open_dir_nofollow("checkpoints")?;
        for (id, edit) in self.records(None)? {
            if edit.seq >= boundary {
                continue;
            }
            let encoded = serde_json::to_vec(&edit)?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = target.open_with(format!("{id}.json"), &options)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            let mut confirmation = target.open_with(format!("{id}.ok"), &options)?;
            confirmation.write_all(format!("{:x}", Sha256::digest(encoded)).as_bytes())?;
            confirmation.sync_all()?;
        }
        Ok(())
    }

    /// Restore confirmed edits at/after an event boundary. All paths and edit
    /// chains are validated first. Any unrelated edit refuses the entire plan.
    /// Each replaced postimage remains in a sibling recovery file even on I/O
    /// failure; an interrupted multi-file restore can therefore be recovered.
    /// # Errors
    /// Invalid records, intervening edits, unsafe paths, or filesystem failures.
    pub fn restore(&self, boundary: u64) -> io::Result<usize> {
        let mut edits = self
            .records(Some(boundary))?
            .into_iter()
            .map(|(_, edit)| edit)
            .filter(|edit| edit.seq >= boundary)
            .collect::<Vec<_>>();
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.seq));
        let mut plan: BTreeMap<PathBuf, (Option<String>, Option<String>)> = BTreeMap::new();
        for edit in edits {
            let row = match plan.entry(edit.path.clone()) {
                std::collections::btree_map::Entry::Occupied(row) => row.into_mut(),
                std::collections::btree_map::Entry::Vacant(row) => {
                    let current = read(&self.workspace, &edit.path)?;
                    row.insert((current.clone(), current))
                }
            };
            if row.1.as_ref() != Some(&edit.after) {
                return Err(invalid(&format!(
                    "rewind conflict: {} changed outside the recorded edit chain; no files restored",
                    edit.path.display()
                )));
            }
            row.1 = edit.before;
        }
        let count = plan.len();
        for (path, (expected, before)) in plan {
            let (dir, name) = parent(&self.workspace, &path)?;
            let recovery = format!(".heycode-rewind-{}", uuid::Uuid::new_v4());
            // Rename preserves the actual inode (including an editor's racing
            // write) before checking it. Never truncate an existing target.
            dir.rename(&name, &dir, &recovery)?;
            if read(&dir, Path::new(&recovery))? != expected {
                // No-clobber restoration; retain recovery if another writer won.
                if dir.hard_link(&recovery, &dir, &name).is_ok() {
                    dir.remove_file(&recovery)?;
                }
                return Err(invalid(&format!(
                    "rewind raced with an edit; original preserved at {recovery}"
                )));
            }
            if let Some(before) = before {
                let staged = format!(".heycode-restore-{}", uuid::Uuid::new_v4());
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                let mut file = dir.open_with(&staged, &options)?;
                file.set_permissions(dir.metadata(&recovery)?.permissions())?;
                file.write_all(before.as_bytes())?;
                file.sync_all()?;
                if let Err(error) = dir.hard_link(&staged, &dir, &name) {
                    return Err(invalid(&format!(
                        "restore stopped: {error}; postimage preserved at {recovery}, preimage at {staged}"
                    )));
                }
                dir.remove_file(staged)?;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn candidate_counts_are_read_only_bounded_by_event_and_never_invent_eligibility() {
        let session = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        assert_eq!(
            Checkpoints::restore_candidate_counts(session.path(), work.path(), &[0, 11]).unwrap(),
            vec![0, 0]
        );
        assert!(
            !session.path().join("checkpoints").exists(),
            "preview created an empty checkpoint store"
        );
        std::fs::write(work.path().join("a"), "old").unwrap();
        let store = Checkpoints::open(session.path(), work.path()).unwrap();
        let edit = store.prepare(10, Path::new("a"), "new".into()).unwrap();
        std::fs::write(work.path().join("a"), "new").unwrap();
        assert!(
            Checkpoints::restore_candidate_counts(session.path(), work.path(), &[0, 11]).is_err(),
            "uncertain edit was advertised as restorable"
        );
        store.confirm(edit).unwrap();
        assert_eq!(
            Checkpoints::restore_candidate_counts(session.path(), work.path(), &[0, 10, 11])
                .unwrap(),
            vec![1, 1, 0]
        );
        assert_eq!(
            std::fs::read_to_string(work.path().join("a")).unwrap(),
            "new"
        );
        std::fs::write(session.path().join("checkpoints/corrupt.json"), "invalid").unwrap();
        assert!(Checkpoints::restore_candidate_counts(session.path(), work.path(), &[0]).is_err());
        assert_eq!(
            std::fs::read_to_string(work.path().join("a")).unwrap(),
            "new"
        );
    }

    #[test]
    fn durable_restore_preserves_unrelated_files_and_refuses_conflicts() {
        let session = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("a"), "old").unwrap();
        std::fs::write(work.path().join("user"), "precious").unwrap();
        let store = Checkpoints::open(session.path(), work.path()).unwrap();
        let pending = store.prepare(10, Path::new("a"), "agent".into()).unwrap();
        std::fs::write(work.path().join("a"), "agent").unwrap();
        store.confirm(pending).unwrap();
        drop(store);
        let store = Checkpoints::open(session.path(), work.path()).unwrap();
        std::fs::write(work.path().join("a"), "user edit").unwrap();
        assert!(store.restore(10).is_err());
        assert_eq!(
            std::fs::read_to_string(work.path().join("a")).unwrap(),
            "user edit"
        );
        std::fs::write(work.path().join("a"), "agent").unwrap();
        assert_eq!(store.restore(10).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(work.path().join("a")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(work.path().join("user")).unwrap(),
            "precious"
        );
    }
    #[test]
    fn unconfirmed_and_traversal_are_never_restored() {
        let session = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = Checkpoints::open(session.path(), work.path()).unwrap();
        assert!(
            store
                .prepare(0, Path::new("../escape"), "x".into())
                .is_err()
        );
        let _pending = store.prepare(0, Path::new("new"), "new".into()).unwrap();
        std::fs::write(work.path().join("new"), "new").unwrap();
        assert!(store.restore(0).is_err());
    }
    #[test]
    fn all_paths_are_checked_before_restore_and_intervening_user_edits_refuse() {
        let session = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = Checkpoints::open(session.path(), work.path()).unwrap();
        for (seq, path, before, after) in [
            (1, "a", "original-a", "agent-a"),
            (2, "b", "original-b", "agent-b"),
        ] {
            std::fs::write(work.path().join(path), before).unwrap();
            let pending = store.prepare(seq, Path::new(path), after.into()).unwrap();
            std::fs::write(work.path().join(path), after).unwrap();
            store.confirm(pending).unwrap();
        }
        std::fs::write(work.path().join("b"), "user-b").unwrap();
        assert!(store.restore(0).is_err());
        assert_eq!(
            std::fs::read_to_string(work.path().join("a")).unwrap(),
            "agent-a"
        );
        let pending = store
            .prepare(3, Path::new("b"), "later-agent-b".into())
            .unwrap();
        std::fs::write(work.path().join("b"), "later-agent-b").unwrap();
        store.confirm(pending).unwrap();
        assert!(store.restore(0).is_err());
        assert_eq!(
            std::fs::read_to_string(work.path().join("b")).unwrap(),
            "later-agent-b"
        );
    }

    #[test]
    fn copied_prefix_remains_restorable_and_corrupt_record_is_rejected() {
        let session = tempfile::tempdir().unwrap();
        let child = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let store = Checkpoints::open(session.path(), work.path()).unwrap();
        std::fs::write(work.path().join("a"), "old").unwrap();
        let pending = store.prepare(1, Path::new("a"), "new".into()).unwrap();
        let id = pending.name.clone();
        std::fs::write(work.path().join("a"), "new").unwrap();
        store.confirm(pending).unwrap();
        store.copy_prefix_to(child.path(), 2).unwrap();
        let copied = Checkpoints::open(child.path(), work.path()).unwrap();
        assert_eq!(copied.restore(1).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(work.path().join("a")).unwrap(),
            "old"
        );
        std::fs::write(
            session
                .path()
                .join("checkpoints")
                .join(format!("{id}.json")),
            r#"{"version":1,"seq":1,"path":"a","before":"forged","after":"old"}"#,
        )
        .unwrap();
        assert!(store.restore(0).is_err());
        assert_eq!(
            std::fs::read_to_string(work.path().join("a")).unwrap(),
            "old"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_paths_refused() {
        let session = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), work.path().join("link")).unwrap();
        let store = Checkpoints::open(session.path(), work.path()).unwrap();
        assert!(
            store
                .prepare(0, Path::new("link/file"), "x".into())
                .is_err()
        );
    }
}
