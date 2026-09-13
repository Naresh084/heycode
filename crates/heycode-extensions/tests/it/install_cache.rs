//! PL02 content-addressed plugin cache product contracts.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use heycode_extensions::{
    ApiVersion, Architecture, CacheCorruption, InstallDisposition, ManifestValidator,
    OperatingSystem, PLUGIN_CACHE_SCHEMA_VERSION, PackageCacheError, PackageSourceIssue,
    PlatformTarget, PluginInstallCache,
};

fn validator() -> ManifestValidator {
    ManifestValidator::new(
        ApiVersion::new(1).unwrap(),
        PlatformTarget::new(OperatingSystem::Macos, Architecture::Aarch64),
    )
}

fn manifest(id: &str, version: &str, contribution_path: &str) -> String {
    format!(
        r#"schema_version = 1
id = "{id}"
name = "Cache fixture"
version = "{version}"
description = "A deterministic local cache fixture."
license = "MIT"
default_enabled = true
requested_permissions = []
platforms = [{{ os = "macos", architecture = "aarch64" }}]
dependencies = []
conflicts = []
contributions = [{{ kind = "skill", id = "review", path = "{contribution_path}", exposure = {{ mode = "namespaced" }} }}]

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixtures/cache-package"
revision = "{version}"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"#,
    )
}

fn write_package(root: &Path, id: &str, version: &str, body: &str) {
    std::fs::create_dir_all(root.join(".heycode-plugin")).unwrap();
    std::fs::create_dir_all(root.join("skills/review")).unwrap();
    std::fs::create_dir_all(root.join("assets")).unwrap();
    std::fs::write(
        root.join(".heycode-plugin/plugin.toml"),
        manifest(id, version, "skills/review/SKILL.md"),
    )
    .unwrap();
    std::fs::write(root.join("skills/review/SKILL.md"), body).unwrap();
    std::fs::write(root.join("assets/helper.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            root.join("assets/helper.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
}

fn cache(temp: &tempfile::TempDir) -> PluginInstallCache {
    PluginInstallCache::open(temp.path().join("cache"), validator()).unwrap()
}

fn files_below(path: &Path) -> Vec<PathBuf> {
    fn walk(path: &Path, result: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut entries = entries.map(Result::unwrap).collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let child = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(&child, result);
            } else {
                result.push(child);
            }
        }
    }
    let mut result = Vec::new();
    walk(path, &mut result);
    result
}

#[test]
fn install_is_content_addressed_owner_only_and_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source, "local/cache-fixture", "1.0.0", "# Review\n");
    let cache = cache(&temp);

    let installed = cache.install_directory(&source).unwrap();
    assert_eq!(installed.disposition(), InstallDisposition::Installed);
    assert_eq!(installed.manifest().id().as_str(), "local/cache-fixture");
    assert_eq!(installed.manifest().version().as_str(), "1.0.0");
    assert_eq!(
        installed.content_hash().as_str(),
        "sha256:cebdcf8fed1ddd4036ba31f0ccb82864aba2febc0877ba0293ffd5f9590a42d2"
    );
    assert!(installed.package_root().starts_with(cache.root()));
    assert_eq!(
        std::fs::read(installed.package_root().join("skills/review/SKILL.md")).unwrap(),
        b"# Review\n"
    );

    let repeated = cache.install_directory(&source).unwrap();
    assert_eq!(repeated.disposition(), InstallDisposition::AlreadyPresent);
    assert_eq!(repeated.content_hash(), installed.content_hash());
    assert_eq!(repeated.package_root(), installed.package_root());
    let resolved = cache
        .resolve(installed.manifest().id(), installed.manifest().version())
        .unwrap();
    assert_eq!(resolved.content_hash(), installed.content_hash());
    assert_eq!(resolved.package_root(), installed.package_root());

    let identical_source = temp.path().join("identical-source");
    write_package(
        &identical_source,
        "local/cache-fixture",
        "1.0.0",
        "# Review\n",
    );
    let identical = cache.install_directory(&identical_source).unwrap();
    assert_eq!(identical.disposition(), InstallDisposition::AlreadyPresent);
    assert_eq!(identical.content_hash(), installed.content_hash());

    let snapshot = cache.inspect().unwrap();
    assert_eq!(snapshot.schema_version, PLUGIN_CACHE_SCHEMA_VERSION);
    assert_eq!(snapshot.packages.len(), 1);
    assert_eq!(snapshot.packages[0].id.as_str(), "local/cache-fixture");
    assert_eq!(snapshot.packages[0].version.as_str(), "1.0.0");
    assert_eq!(snapshot.packages[0].content_hash, *installed.content_hash());
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains(temp.path().to_str().unwrap())
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(cache.root())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(cache.root().join(".cache-schema"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(installed.package_root().join(".heycode-plugin/plugin.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(installed.package_root().join("assets/helper.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn prior_versions_are_retained_and_same_version_substitution_fails() {
    let temp = tempfile::tempdir().unwrap();
    let source_v1 = temp.path().join("source-v1");
    let source_v2 = temp.path().join("source-v2");
    let conflict = temp.path().join("conflict");
    write_package(&source_v1, "local/cache-fixture", "1.0.0", "one\n");
    write_package(&source_v2, "local/cache-fixture", "2.0.0", "two\n");
    write_package(&conflict, "local/cache-fixture", "1.0.0", "substitution\n");
    let cache = cache(&temp);

    let first = cache.install_directory(&source_v1).unwrap();
    let second = cache.install_directory(&source_v2).unwrap();
    assert_ne!(first.content_hash(), second.content_hash());
    assert!(first.package_root().is_dir());
    assert!(second.package_root().is_dir());

    assert!(matches!(
        cache.install_directory(&conflict),
        Err(PackageCacheError::VersionConflict { .. })
    ));
    let snapshot = cache.inspect().unwrap();
    assert_eq!(
        snapshot
            .packages
            .iter()
            .map(|package| package.version.as_str())
            .collect::<Vec<_>>(),
        ["1.0.0", "2.0.0"]
    );
    assert_eq!(
        std::fs::read(first.package_root().join("skills/review/SKILL.md")).unwrap(),
        b"one\n"
    );
    assert_eq!(
        cache
            .cleanup_stale(Duration::ZERO)
            .unwrap()
            .orphan_objects_removed,
        0
    );
    assert!(first.package_root().is_dir());
    assert!(second.package_root().is_dir());
}

#[test]
fn manifest_and_declared_package_paths_are_validated_before_staging() {
    let temp = tempfile::tempdir().unwrap();
    let missing_manifest = temp.path().join("missing-manifest");
    std::fs::create_dir_all(&missing_manifest).unwrap();
    let missing_contribution = temp.path().join("missing-contribution");
    std::fs::create_dir_all(missing_contribution.join(".heycode-plugin")).unwrap();
    std::fs::write(
        missing_contribution.join(".heycode-plugin/plugin.toml"),
        manifest("local/cache-fixture", "1.0.0", "skills/review/SKILL.md"),
    )
    .unwrap();
    let cache = cache(&temp);
    let missing_schema = temp.path().join("missing-schema");
    write_package(&missing_schema, "local/missing-schema", "1.0.0", "body\n");
    let manifest_path = missing_schema.join(".heycode-plugin/plugin.toml");
    let with_schema = std::fs::read_to_string(&manifest_path).unwrap().replace(
        "default_enabled = true",
        "default_enabled = true\nconfiguration_schema = \"schemas/missing.json\"",
    );
    std::fs::write(manifest_path, with_schema).unwrap();

    assert!(matches!(
        cache.install_directory(&missing_manifest),
        Err(PackageCacheError::MissingManifest)
    ));
    assert!(matches!(
        cache.install_directory(&missing_contribution),
        Err(PackageCacheError::MissingDeclaredPath)
    ));
    assert!(matches!(
        cache.install_directory(&missing_schema),
        Err(PackageCacheError::MissingDeclaredPath)
    ));
    assert!(cache.inspect().unwrap().packages.is_empty());
    assert!(files_below(&cache.root().join(".objects")).is_empty());
    assert!(files_below(&cache.root().join(".refs")).is_empty());
}

#[cfg(unix)]
#[test]
fn source_symlinks_hardlinks_and_nonportable_names_fail_closed() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let cache = cache(&temp);

    let symlinked = temp.path().join("symlinked");
    write_package(&symlinked, "local/symlinked", "1.0.0", "body\n");
    let external = temp.path().join("outside");
    std::fs::write(&external, b"outside").unwrap();
    std::fs::remove_file(symlinked.join("skills/review/SKILL.md")).unwrap();
    symlink(&external, symlinked.join("skills/review/SKILL.md")).unwrap();
    assert!(matches!(
        cache.install_directory(&symlinked),
        Err(PackageCacheError::UnsafeSource {
            issue: PackageSourceIssue::SymbolicLink
        })
    ));

    let hardlinked = temp.path().join("hardlinked");
    write_package(&hardlinked, "local/hardlinked", "1.0.0", "body\n");
    std::fs::remove_file(hardlinked.join("skills/review/SKILL.md")).unwrap();
    std::fs::hard_link(&external, hardlinked.join("skills/review/SKILL.md")).unwrap();
    assert!(matches!(
        cache.install_directory(&hardlinked),
        Err(PackageCacheError::UnsafeSource {
            issue: PackageSourceIssue::HardLink
        })
    ));

    let nonportable = temp.path().join("nonportable");
    write_package(&nonportable, "local/nonportable", "1.0.0", "body\n");
    std::fs::write(nonportable.join("assets/bad:name"), b"bad").unwrap();
    assert!(matches!(
        cache.install_directory(&nonportable),
        Err(PackageCacheError::UnsafeSource {
            issue: PackageSourceIssue::NonPortablePath
        })
    ));

    let real = temp.path().join("real-root");
    write_package(&real, "local/root-link", "1.0.0", "body\n");
    let root_link = temp.path().join("root-link");
    symlink(&real, &root_link).unwrap();
    assert!(matches!(
        cache.install_directory(&root_link),
        Err(PackageCacheError::UnsafeSource {
            issue: PackageSourceIssue::SymbolicLink
        })
    ));
}

#[test]
fn concurrent_same_package_installs_converge_once() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source, "local/concurrent", "1.0.0", "body\n");
    let cache = Arc::new(cache(&temp));
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let barrier = Arc::clone(&barrier);
        let source = source.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            cache.install_directory(source)
        }));
    }
    let receipts = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.disposition() == InstallDisposition::Installed)
            .count(),
        1
    );
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.content_hash() == receipts[0].content_hash())
    );
    assert_eq!(cache.inspect().unwrap().packages.len(), 1);
    assert!(files_below(&cache.root().join(".staging")).is_empty());
}

#[test]
fn concurrent_same_version_substitution_has_one_winner_and_no_orphan_object() {
    let temp = tempfile::tempdir().unwrap();
    let left = temp.path().join("left");
    let right = temp.path().join("right");
    write_package(&left, "local/racing", "1.0.0", "left\n");
    write_package(&right, "local/racing", "1.0.0", "right\n");
    let cache = Arc::new(cache(&temp));
    let barrier = Arc::new(Barrier::new(2));
    let workers = [left, right]
        .into_iter()
        .map(|source| {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                cache.install_directory(source)
            })
        })
        .collect::<Vec<_>>();
    let outcomes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    let success_count = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    assert_eq!(success_count, 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Err(PackageCacheError::VersionConflict { .. })))
            .count(),
        1
    );
    assert_eq!(cache.inspect().unwrap().packages.len(), 1);
    let object_directories = std::fs::read_dir(cache.root().join(".objects/sha256"))
        .unwrap()
        .count();
    assert_eq!(object_directories, 1);
    assert!(files_below(&cache.root().join(".staging")).is_empty());
}

#[test]
fn concurrent_cleanup_cannot_break_live_conflicting_publishers() {
    let temp = tempfile::tempdir().unwrap();
    let left = temp.path().join("cleanup-left");
    let right = temp.path().join("cleanup-right");
    write_package(&left, "local/cleanup-race", "1.0.0", "left\n");
    write_package(&right, "local/cleanup-race", "1.0.0", "right\n");
    std::fs::write(left.join("assets/padding"), vec![1_u8; 4 * 1024 * 1024]).unwrap();
    std::fs::write(right.join("assets/padding"), vec![2_u8; 4 * 1024 * 1024]).unwrap();
    let cache = Arc::new(cache(&temp));
    let barrier = Arc::new(Barrier::new(3));
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut workers = Vec::new();
    for source in [left, right] {
        let cache = Arc::clone(&cache);
        let barrier = Arc::clone(&barrier);
        let sender = sender.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            sender.send(cache.install_directory(source)).unwrap();
        }));
    }
    drop(sender);
    barrier.wait();
    let mut outcomes = Vec::new();
    while outcomes.len() < 2 {
        cache.cleanup_stale(Duration::ZERO).unwrap();
        outcomes.extend(receiver.try_iter());
        std::thread::yield_now();
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let success_count = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    let errors = outcomes
        .into_iter()
        .filter_map(Result::err)
        .collect::<Vec<_>>();
    assert!(
        success_count == 1
            && matches!(
                errors.as_slice(),
                [PackageCacheError::VersionConflict { .. }]
            ),
        "unexpected outcomes: {errors:?}"
    );
    assert_eq!(cache.inspect().unwrap().packages.len(), 1);
    assert_eq!(
        std::fs::read_dir(cache.root().join(".objects/sha256"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn case_distinct_semver_text_uses_distinct_portable_reference_keys() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    write_package(&lower, "local/version-case", "1.0.0-alpha", "lower\n");
    write_package(&upper, "local/version-case", "1.0.0-ALPHA", "upper\n");
    let cache = cache(&temp);
    cache.install_directory(&lower).unwrap();
    cache.install_directory(&upper).unwrap();

    let snapshot = cache.inspect().unwrap();
    assert_eq!(snapshot.packages.len(), 2);
    assert_eq!(
        snapshot
            .packages
            .iter()
            .map(|package| package.version.as_str())
            .collect::<Vec<_>>(),
        ["1.0.0-ALPHA", "1.0.0-alpha"]
    );
    let names = files_below(&cache.root().join(".refs"))
        .into_iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert!(names.iter().all(|name| {
        name.len() == 68
            && name.ends_with(".ref")
            && name[..64]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }));
}

#[test]
fn failed_install_keeps_last_good_and_leaves_no_partial_generation() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let conflict = temp.path().join("conflict");
    let invalid = temp.path().join("invalid");
    write_package(&source, "local/failure", "1.0.0", "good\n");
    write_package(&conflict, "local/failure", "1.0.0", "different\n");
    write_package(&invalid, "local/failure", "2.0.0", "invalid\n");
    std::fs::remove_file(invalid.join("skills/review/SKILL.md")).unwrap();
    let cache = cache(&temp);
    let good = cache.install_directory(&source).unwrap();

    assert!(cache.install_directory(&conflict).is_err());
    assert!(cache.install_directory(&invalid).is_err());
    let snapshot = cache.inspect().unwrap();
    assert_eq!(snapshot.packages.len(), 1);
    assert_eq!(snapshot.packages[0].content_hash, *good.content_hash());
    assert_eq!(
        std::fs::read(good.package_root().join("skills/review/SKILL.md")).unwrap(),
        b"good\n"
    );
    assert!(files_below(&cache.root().join(".staging")).is_empty());
}

#[test]
fn inspection_detects_tampering_instead_of_trusting_refs() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source, "local/tamper", "1.0.0", "original\n");
    let cache = cache(&temp);
    let installed = cache.install_directory(&source).unwrap();
    std::fs::write(
        installed.package_root().join("skills/review/SKILL.md"),
        b"tampered\n",
    )
    .unwrap();

    assert!(matches!(
        cache.inspect(),
        Err(PackageCacheError::CorruptCache {
            reason: CacheCorruption::ContentHashMismatch
        })
    ));
}

#[test]
fn cache_hardlinks_and_symlinks_are_corruption_not_activation_paths() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source, "local/cache-tamper", "1.0.0", "original\n");
    let cache = cache(&temp);
    let installed = cache.install_directory(&source).unwrap();
    let reference = files_below(&cache.root().join(".refs"))
        .into_iter()
        .next()
        .unwrap();
    let second_name = temp.path().join("reference-hardlink");
    std::fs::hard_link(&reference, &second_name).unwrap();
    assert!(matches!(
        cache.inspect(),
        Err(PackageCacheError::CorruptCache {
            reason: CacheCorruption::HardLink
        })
    ));
    std::fs::remove_file(second_name).unwrap();

    let object_file = installed.package_root().join("skills/review/SKILL.md");
    let outside = temp.path().join("outside-object");
    std::fs::write(&outside, b"outside").unwrap();
    std::fs::remove_file(&object_file).unwrap();
    symlink(&outside, &object_file).unwrap();
    assert!(matches!(
        cache.inspect(),
        Err(PackageCacheError::CorruptCache {
            reason: CacheCorruption::UnsafeFileType
        })
    ));
}

#[test]
fn cache_root_must_be_absolute_real_and_owner_only() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let temp = tempfile::tempdir().unwrap();
    assert!(matches!(
        PluginInstallCache::open("relative-cache", validator()),
        Err(PackageCacheError::InvalidRoot)
    ));

    let broad = temp.path().join("broad");
    std::fs::create_dir(&broad).unwrap();
    std::fs::set_permissions(&broad, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        PluginInstallCache::open(&broad, validator()),
        Err(PackageCacheError::CorruptCache {
            reason: CacheCorruption::Permissions
        })
    ));

    let unrelated = temp.path().join("unrelated");
    std::fs::create_dir(&unrelated).unwrap();
    std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(unrelated.join("not-a-cache"), b"data").unwrap();
    assert!(matches!(
        PluginInstallCache::open(&unrelated, validator()),
        Err(PackageCacheError::CorruptCache {
            reason: CacheCorruption::UnexpectedEntry
        })
    ));

    let real = temp.path().join("real-cache");
    std::fs::create_dir(&real).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
    let linked = temp.path().join("linked-cache");
    symlink(&real, &linked).unwrap();
    assert!(matches!(
        PluginInstallCache::open(&linked, validator()),
        Err(PackageCacheError::CorruptCache {
            reason: CacheCorruption::UnsafeFileType
        })
    ));
}

#[test]
fn cache_schema_marker_is_strict_and_future_versions_fail_loud() {
    let temp = tempfile::tempdir().unwrap();
    let cache = cache(&temp);
    std::fs::write(cache.root().join(".cache-schema"), b"schema_version = 2\n").unwrap();
    assert!(matches!(
        PluginInstallCache::open(cache.root(), validator()),
        Err(PackageCacheError::UnsupportedCacheSchema {
            found: 2,
            supported: PLUGIN_CACHE_SCHEMA_VERSION
        })
    ));
}

#[test]
fn concurrent_first_open_publishes_one_complete_schema_generation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("concurrent-open");
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let root = root.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            PluginInstallCache::open(root, validator())
        }));
    }
    for worker in workers {
        assert!(worker.join().unwrap().is_ok());
    }
    assert_eq!(
        std::fs::read_to_string(root.join(".cache-schema")).unwrap(),
        format!("schema_version = {PLUGIN_CACHE_SCHEMA_VERSION}\n")
    );
    assert!(!root.join(".cache-init.lock").exists());
    let reopened = PluginInstallCache::open(&root, validator()).unwrap();
    assert!(reopened.inspect().unwrap().packages.is_empty());
}

#[test]
fn individual_package_files_have_a_hard_admission_cap() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source, "local/oversized", "1.0.0", "body\n");
    std::fs::write(
        source.join("assets/large.bin"),
        vec![0_u8; 16 * 1024 * 1024 + 1],
    )
    .unwrap();
    let cache = cache(&temp);
    assert!(matches!(
        cache.install_directory(&source),
        Err(PackageCacheError::PackageLimit {
            limit: heycode_extensions::PackageLimit::FileBytes
        })
    ));
    assert!(cache.inspect().unwrap().packages.is_empty());
}

#[test]
fn abandoned_staging_and_locks_have_explicit_bounded_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let cache = cache(&temp);
    let abandoned = cache.root().join(".staging/abandoned-test");
    std::fs::create_dir(&abandoned).unwrap();
    std::fs::write(abandoned.join("partial"), b"partial").unwrap();
    let lock_parent = cache.root().join(".locks/object");
    let abandoned_lock = lock_parent.join("dead-test.lock");
    std::fs::write(&abandoned_lock, b"").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&abandoned, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(
        abandoned.join("partial"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::set_permissions(&abandoned_lock, std::fs::Permissions::from_mode(0o600)).unwrap();

    let report = cache.cleanup_stale(Duration::ZERO).unwrap();
    assert_eq!(report.staging_directories_removed, 1);
    assert_eq!(report.lock_files_removed, 1);
    assert_eq!(report.orphan_objects_removed, 0);
    assert!(!abandoned.exists());
    assert!(!abandoned_lock.exists());
}

#[test]
#[allow(deprecated)]
fn cleanup_probes_os_locks_and_never_removes_live_stage_or_install_leases() {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::PermissionsExt as _;

    use nix::fcntl::{FlockArg, flock};

    let temp = tempfile::tempdir().unwrap();
    let cache = cache(&temp);
    let stage = cache.root().join(".staging/live-stage");
    let stage_lease = cache.root().join(".staging/live-stage.lease");
    std::fs::create_dir(&stage).unwrap();
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(stage.join("partial"), b"partial").unwrap();
    std::fs::set_permissions(
        stage.join("partial"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(&stage_lease, b"").unwrap();
    std::fs::set_permissions(&stage_lease, std::fs::Permissions::from_mode(0o600)).unwrap();
    let stage_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&stage_lease)
        .unwrap();
    flock(stage_file.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();

    let install_lock = cache.root().join(".locks/object/live-install.lock");
    std::fs::write(&install_lock, b"").unwrap();
    std::fs::set_permissions(&install_lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    let install_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&install_lock)
        .unwrap();
    flock(install_file.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();

    let live = cache.cleanup_stale(Duration::ZERO).unwrap();
    assert_eq!(live.staging_directories_removed, 0);
    assert_eq!(live.lock_files_removed, 0);
    assert!(stage.is_dir());
    assert!(install_lock.is_file());

    flock(stage_file.as_raw_fd(), FlockArg::Unlock).unwrap();
    flock(install_file.as_raw_fd(), FlockArg::Unlock).unwrap();
    drop((stage_file, install_file));
    let stale = cache.cleanup_stale(Duration::ZERO).unwrap();
    assert_eq!(stale.staging_directories_removed, 1);
    assert!(stale.lock_files_removed >= 1);
    assert!(!stage.exists());
    assert!(!install_lock.exists());
}

#[test]
fn abandoned_complete_objects_are_recoverable_but_referenced_versions_are_retained() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_package(&source, "local/orphan", "1.0.0", "body\n");
    let cache = cache(&temp);
    let installed = cache.install_directory(&source).unwrap();
    let reference = files_below(&cache.root().join(".refs"))
        .into_iter()
        .next()
        .unwrap();
    std::fs::remove_file(reference).unwrap();

    let report = cache.cleanup_stale(Duration::ZERO).unwrap();
    assert_eq!(report.orphan_objects_removed, 1);
    assert!(!installed.package_root().exists());
    assert!(cache.inspect().unwrap().packages.is_empty());
}

#[test]
fn cleanup_is_capability_bounded_and_never_follows_staging_symlinks() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let temp = tempfile::tempdir().unwrap();
    let cache = cache(&temp);
    let outside = temp.path().join("outside-cleanup");
    std::fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("keep");
    std::fs::write(&sentinel, b"keep").unwrap();
    let stage = cache.root().join(".staging/symlink-stage");
    std::fs::create_dir(&stage).unwrap();
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&outside, stage.join("escape")).unwrap();

    let report = cache.cleanup_stale(Duration::ZERO).unwrap();
    assert_eq!(report.staging_directories_removed, 1);
    assert!(sentinel.is_file());
    assert!(!stage.exists());
}
