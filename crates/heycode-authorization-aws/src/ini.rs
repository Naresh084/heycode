//! Minimal reader for the shared AWS `config` and `credentials` files.
//!
//! Deliberately narrow. The reader answers two questions — *which settings
//! does this profile define* and *what is the value of one named safe
//! setting* — and it is private to this crate so no caller can use it to lift
//! `aws_secret_access_key` out of a file into a returned data structure.

use std::collections::BTreeSet;

/// Which of the two shared files is being read.
///
/// They differ only in how a profile heads a section: `config` prefixes every
/// non-default profile with `profile `, `credentials` never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedFile {
    /// `~/.aws/config`.
    Config,
    /// `~/.aws/credentials`.
    Credentials,
}

const DEFAULT_PROFILE: &str = "default";

fn heads_profile(header: &str, profile: &str, file: SharedFile) -> bool {
    match file {
        SharedFile::Credentials => header == profile,
        SharedFile::Config => {
            header == format!("profile {profile}")
                || (profile == DEFAULT_PROFILE && header == DEFAULT_PROFILE)
        }
    }
}

/// Walk the `key = value` lines of one profile's section.
///
/// Indented lines are AWS sub-properties of the preceding key; they are
/// skipped so a nested block cannot be mistaken for a top-level setting.
fn entries<'a>(
    text: &'a str,
    profile: &'a str,
    file: SharedFile,
) -> impl Iterator<Item = (String, &'a str)> {
    let mut inside = false;
    text.lines().filter_map(move |raw| {
        let indented = raw.starts_with(' ') || raw.starts_with('\t');
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            return None;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = heads_profile(header.trim(), profile, file);
            return None;
        }
        if !inside || indented {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        Some((key.trim().to_ascii_lowercase(), value.trim()))
    })
}

/// Names of the settings one profile defines, or `None` when the file has no
/// section for it. Values are never returned.
pub(crate) fn section_keys(
    text: &str,
    profile: &str,
    file: SharedFile,
) -> Option<BTreeSet<String>> {
    let mut found = false;
    let mut keys = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']'))
            && heads_profile(header.trim(), profile, file)
        {
            found = true;
        }
    }
    if !found {
        return None;
    }
    for (key, _) in entries(text, profile, file) {
        keys.insert(key);
    }
    Some(keys)
}

/// The value of one named setting in one profile.
///
/// Callers pass only settings that are safe to surface; the module doc states
/// why that restriction is a boundary rather than a convention.
pub(crate) fn section_value(
    text: &str,
    profile: &str,
    file: SharedFile,
    key: &str,
) -> Option<String> {
    entries(text, profile, file)
        .find(|(found, _)| found == key)
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty())
}

/// Environment variable that overrides the shared `config` path.
pub(crate) const CONFIG_FILE_VAR: &str = "AWS_CONFIG_FILE";
/// Environment variable that overrides the shared `credentials` path.
pub(crate) const CREDENTIALS_FILE_VAR: &str = "AWS_SHARED_CREDENTIALS_FILE";

/// Resolve one shared file's path: explicit override first, then the
/// documented `~/.aws/` location. `None` means neither is knowable.
pub(crate) fn path(host: &dyn crate::AwsHost, file: SharedFile) -> Option<std::path::PathBuf> {
    let (variable, leaf) = match file {
        SharedFile::Config => (CONFIG_FILE_VAR, "config"),
        SharedFile::Credentials => (CREDENTIALS_FILE_VAR, "credentials"),
    };
    if let Some(explicit) = host.var(variable) {
        return Some(std::path::PathBuf::from(explicit));
    }
    Some(host.home()?.join(".aws").join(leaf))
}

/// Read one shared file through the host boundary.
pub(crate) fn read(host: &dyn crate::AwsHost, file: SharedFile) -> crate::AwsFileRead {
    match path(host, file) {
        Some(path) => host.read_file(&path),
        None => crate::AwsFileRead::Absent,
    }
}
