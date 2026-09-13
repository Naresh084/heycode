//! Bound executable/interpreter resolution for both Codex launches.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use heycode_exec::SubprocessService;
use sha2::{Digest as _, Sha256};

use crate::{CodexAppServerError, CodexAppServerErrorCode};

const MAX_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHEBANG_BYTES: usize = 4 * 1024;
type SanitizedEnvironment = (Vec<(OsString, OsString)>, Vec<PathBuf>);

pub(crate) struct BoundLaunch {
    program: PathBuf,
    argument_prefix: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    identity: LaunchIdentity,
}

impl BoundLaunch {
    pub(crate) fn resolve(
        requested_program: &OsStr,
        cwd: &Path,
        environment: &[(OsString, OsString)],
        subprocess: &SubprocessService,
    ) -> Result<Self, CodexAppServerError> {
        let canonical_cwd = std::fs::canonicalize(cwd).map_err(|_| invalid_config())?;
        let (environment, path_roots) = sanitize_environment(environment, &canonical_cwd)?;
        let requested_path = Path::new(requested_program);
        let bare_name = is_bare_name(requested_path);
        if !requested_path.is_absolute() && !bare_name {
            return Err(invalid_config());
        }
        if bare_name && path_roots.is_empty() {
            return Err(invalid_config());
        }
        let resolved = subprocess
            .resolve_program(requested_program)
            .map_err(CodexAppServerError::from)?;
        let executable = canonical_executable(&resolved, &canonical_cwd)?;
        if bare_name && !selected_from_path(&resolved, &path_roots) {
            return Err(invalid_config());
        }

        let shebang = read_shebang(&executable)?;
        let (program, argument_prefix, identities) = match shebang {
            None => (
                executable.clone(),
                Vec::new(),
                vec![ExecutableIdentity::capture(executable)?],
            ),
            Some(Shebang::Direct(interpreter)) => {
                let interpreter = native_interpreter(&interpreter, &canonical_cwd)?;
                (
                    interpreter.clone(),
                    vec![executable.as_os_str().to_os_string()],
                    vec![
                        ExecutableIdentity::capture(executable)?,
                        ExecutableIdentity::capture(interpreter)?,
                    ],
                )
            }
            Some(Shebang::Environment(command)) => {
                let interpreter = resolve_native_in_path(&command, &path_roots, &canonical_cwd)?;
                (
                    interpreter.clone(),
                    vec![executable.as_os_str().to_os_string()],
                    vec![
                        ExecutableIdentity::capture(executable)?,
                        ExecutableIdentity::capture(interpreter)?,
                    ],
                )
            }
        };
        Ok(Self {
            program,
            argument_prefix,
            environment,
            identity: LaunchIdentity(identities),
        })
    }

    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    pub(crate) fn args_with(&self, suffix: &[&str]) -> Vec<OsString> {
        self.argument_prefix
            .iter()
            .cloned()
            .chain(suffix.iter().map(OsString::from))
            .collect()
    }

    pub(crate) fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    pub(crate) fn identity(&self) -> LaunchIdentity {
        self.identity.clone()
    }
}

fn selected_from_path(resolved: &Path, roots: &[PathBuf]) -> bool {
    resolved
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .is_some_and(|parent| roots.iter().any(|root| root == &parent))
}

impl std::fmt::Debug for BoundLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundLaunch")
            .field("program", &"<redacted>")
            .field("argument_prefix_count", &self.argument_prefix.len())
            .field("environment_count", &self.environment.len())
            .field("identity_count", &self.identity.0.len())
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct LaunchIdentity(Vec<ExecutableIdentity>);

impl LaunchIdentity {
    pub(crate) fn verify(&self) -> Result<(), CodexAppServerError> {
        for identity in &self.0 {
            identity.verify()?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for LaunchIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LaunchIdentity")
            .field("executable_count", &self.0.len())
            .finish()
    }
}

#[derive(Clone)]
struct ExecutableIdentity {
    canonical_path: PathBuf,
    digest: [u8; 32],
}

impl ExecutableIdentity {
    fn capture(path: PathBuf) -> Result<Self, CodexAppServerError> {
        let digest = executable_digest(&path)?;
        Ok(Self {
            canonical_path: path,
            digest,
        })
    }

    fn verify(&self) -> Result<(), CodexAppServerError> {
        let canonical =
            std::fs::canonicalize(&self.canonical_path).map_err(|_| unsupported_version())?;
        if canonical != self.canonical_path
            || executable_digest(&canonical).map_err(|_| unsupported_version())? != self.digest
        {
            return Err(unsupported_version());
        }
        Ok(())
    }
}

fn sanitize_environment(
    environment: &[(OsString, OsString)],
    cwd: &Path,
) -> Result<SanitizedEnvironment, CodexAppServerError> {
    let path_indexes = environment
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| {
            name.to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case("PATH"))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if path_indexes.len() > 1 {
        return Err(invalid_config());
    }
    let mut sanitized = environment.to_vec();
    let Some(index) = path_indexes.first().copied() else {
        return Ok((sanitized, Vec::new()));
    };
    let mut roots = Vec::new();
    let mut unique = BTreeSet::new();
    for root in std::env::split_paths(&sanitized[index].1) {
        if root.as_os_str().is_empty()
            || !root.is_absolute()
            || root
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        {
            return Err(invalid_config());
        }
        let root = match std::fs::canonicalize(root) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(invalid_config()),
        };
        if !root.is_dir() || root.starts_with(cwd) {
            return Err(invalid_config());
        }
        if unique.insert(root.clone()) {
            roots.push(root);
        }
    }
    if roots.is_empty() {
        return Err(invalid_config());
    }
    sanitized[index].1 = std::env::join_paths(&roots).map_err(|_| invalid_config())?;
    Ok((sanitized, roots))
}

fn read_shebang(path: &Path) -> Result<Option<Shebang>, CodexAppServerError> {
    let mut input = std::fs::File::open(path).map_err(|_| invalid_config())?;
    let mut prefix = Vec::new();
    input
        .by_ref()
        .take(u64::try_from(MAX_SHEBANG_BYTES + 1).map_err(|_| invalid_config())?)
        .read_to_end(&mut prefix)
        .map_err(|_| invalid_config())?;
    if !prefix.starts_with(b"#!") {
        return Ok(None);
    }
    let end = prefix
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(prefix.len());
    if end > MAX_SHEBANG_BYTES {
        return Err(invalid_config());
    }
    let line = std::str::from_utf8(&prefix[2..end]).map_err(|_| invalid_config())?;
    let line = line.strip_suffix('\r').unwrap_or(line).trim();
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    match fields.as_slice() {
        [interpreter] if Path::new(interpreter).is_absolute() => {
            Ok(Some(Shebang::Direct(PathBuf::from(interpreter))))
        }
        [environment, command]
            if is_system_env(Path::new(environment)) && valid_command_name(command) =>
        {
            Ok(Some(Shebang::Environment((*command).to_owned())))
        }
        _ => Err(invalid_config()),
    }
}

fn is_system_env(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    std::fs::canonicalize(path).ok().is_some_and(|canonical| {
        [Path::new("/usr/bin/env"), Path::new("/bin/env")]
            .iter()
            .filter_map(|candidate| std::fs::canonicalize(candidate).ok())
            .any(|candidate| candidate == canonical)
    })
}

fn valid_command_name(command: &str) -> bool {
    !command.is_empty()
        && command.len() <= 128
        && command
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn resolve_native_in_path(
    command: &str,
    roots: &[PathBuf],
    cwd: &Path,
) -> Result<PathBuf, CodexAppServerError> {
    for root in roots {
        let candidate = root.join(command);
        if candidate.is_file() {
            return native_interpreter(&candidate, cwd);
        }
    }
    Err(invalid_config())
}

fn native_interpreter(path: &Path, cwd: &Path) -> Result<PathBuf, CodexAppServerError> {
    let interpreter = canonical_executable(path, cwd)?;
    if read_shebang(&interpreter)?.is_some() {
        return Err(invalid_config());
    }
    Ok(interpreter)
}

fn canonical_executable(path: &Path, cwd: &Path) -> Result<PathBuf, CodexAppServerError> {
    let canonical = std::fs::canonicalize(path).map_err(|_| invalid_config())?;
    let metadata = std::fs::metadata(&canonical).map_err(|_| invalid_config())?;
    if !metadata.is_file() || metadata.len() > MAX_EXECUTABLE_BYTES || canonical.starts_with(cwd) {
        return Err(invalid_config());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(invalid_config());
        }
    }
    Ok(canonical)
}

fn executable_digest(path: &Path) -> Result<[u8; 32], CodexAppServerError> {
    let metadata = std::fs::metadata(path).map_err(|_| invalid_config())?;
    if !metadata.is_file() || metadata.len() > MAX_EXECUTABLE_BYTES {
        return Err(invalid_config());
    }
    let mut input = std::fs::File::open(path).map_err(|_| invalid_config())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|_| invalid_config())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn is_bare_name(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().count() == 1
        && matches!(path.components().next(), Some(Component::Normal(_)))
}

enum Shebang {
    Direct(PathBuf),
    Environment(String),
}

fn invalid_config() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::InvalidConfig)
}

fn unsupported_version() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::UnsupportedVersion)
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    #[test]
    fn a_bare_path_entry_may_be_a_symlink_to_an_immutable_package_target() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let package = root.path().join("package");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(&package).unwrap();
        let target = package.join("codex.js");
        std::fs::write(&target, "#!/usr/bin/env node\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let selected = bin.join("codex");
        symlink(&target, &selected).unwrap();
        let canonical_bin = std::fs::canonicalize(&bin).unwrap();

        assert!(selected_from_path(
            &selected,
            std::slice::from_ref(&canonical_bin)
        ));
        assert_ne!(
            std::fs::canonicalize(&selected).unwrap().parent(),
            Some(canonical_bin.as_path()),
            "the old target-parent check rejected normal package-manager shims"
        );
    }
}
