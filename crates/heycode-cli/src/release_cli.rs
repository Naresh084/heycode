//! Q14/Q15 signed local-release management over one plugin-owned service.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use heycode_config::ConfigVersionState;
use heycode_install::{
    GhReleaseManagerConfig, InstallDisposition, PluginApiCompatibility, PluginCompatibilitySet,
    ReleaseApplyOutcome, ReleaseBundle, ReleaseChannel, ReleaseManager, ReleaseManagerError,
    ReleasePlatform, ReleaseTrustPolicy, ReleaseVersion, RollbackOutcome, SERVICE_RELEASE_MANAGER,
    gh_release_manager_plugin,
};

/// Closed release-management operation set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOperation {
    /// Authenticate and apply a fresh install or update.
    Apply,
    /// Re-verify and directionally restore the retained prior version.
    Rollback,
    /// Validate content-free hosted onboarding evidence.
    Evidence,
}

impl ReleaseOperation {
    /// Stable command word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Rollback => "rollback",
            Self::Evidence => "evidence",
        }
    }
}

/// Shared explicit trust/install inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCommonCommand {
    /// Absolute durable installation root.
    pub install_root: PathBuf,
    /// Configured GitHub owner/repository signer identity.
    pub repository: String,
    /// Configured repository-relative signer workflow.
    pub workflow: String,
    /// Exact release tag ref required from the attestation.
    pub source_ref: String,
    /// GitHub CLI name or path resolved by the subprocess provider.
    pub gh_program: OsString,
}

/// Signed local-bundle apply command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseApplyCommand {
    /// Shared install/trust inputs.
    pub common: ReleaseCommonCommand,
    /// Signed release manifest.
    pub manifest: PathBuf,
    /// Manifest attestation bundle.
    pub manifest_bundle: PathBuf,
    /// Platform executable bytes.
    pub artifact: PathBuf,
    /// Executable attestation bundle.
    pub artifact_bundle: PathBuf,
    /// Stable, preview, or exact pin policy.
    pub channel: ReleaseChannel,
}

impl std::ops::Deref for ReleaseApplyCommand {
    type Target = ReleaseCommonCommand;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

/// Parsed release command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseCommand {
    /// Authenticate and install/update one local bundle.
    Apply(ReleaseApplyCommand),
    /// Re-verify and restore the retained prior version.
    Rollback(ReleaseCommonCommand),
    /// Evaluate one or more strict fresh-machine evidence documents.
    Evidence(Vec<PathBuf>),
}

impl ReleaseCommand {
    /// Operation selected by this command.
    #[must_use]
    pub const fn operation(&self) -> ReleaseOperation {
        match self {
            Self::Apply(_) => ReleaseOperation::Apply,
            Self::Rollback(_) => ReleaseOperation::Rollback,
            Self::Evidence(_) => ReleaseOperation::Evidence,
        }
    }

    fn common(&self) -> Option<&ReleaseCommonCommand> {
        match self {
            Self::Apply(command) => Some(&command.common),
            Self::Rollback(command) => Some(command),
            Self::Evidence(_) => None,
        }
    }
}

/// Release command usage.
#[must_use]
pub fn usage() -> String {
    [
        "usage:",
        "  heycode release apply --root <absolute> --manifest <file> --manifest-bundle <file> --artifact <file> --artifact-bundle <file> --repo <owner/repo> --workflow <.github/workflows/file> --source-ref <refs/tags/tag> --channel <stable|preview|pinned:version> [--gh <program>]",
        "  heycode release rollback --root <absolute> --repo <owner/repo> --workflow <.github/workflows/file> --source-ref <refs/tags/tag> [--gh <program>]",
        "  heycode release evidence <absolute-evidence.json> [<absolute-evidence.json> ...]",
    ]
    .join("\n")
}

/// Parse a release-management subcommand without filesystem or composition work.
///
/// # Errors
/// Unknown/duplicate/missing options, relative security-sensitive paths,
/// invalid trust policy, and invalid release channels fail with safe text.
pub fn parse(args: &[String]) -> Result<ReleaseCommand, String> {
    let operation = match args.first().map(String::as_str) {
        Some("apply") => ReleaseOperation::Apply,
        Some("rollback") => ReleaseOperation::Rollback,
        Some("evidence") => ReleaseOperation::Evidence,
        Some(other) => return Err(format!("unknown release operation `{other}`\n{}", usage())),
        None => return Err(usage()),
    };
    if operation == ReleaseOperation::Evidence {
        let mut paths = args[1..].iter().map(PathBuf::from).collect::<Vec<_>>();
        if paths.is_empty() || paths.len() > 16 || paths.iter().any(|path| !path.is_absolute()) {
            return Err("release evidence requires 1..=16 absolute files".to_owned());
        }
        paths.sort();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("release evidence contains a duplicate file".to_owned());
        }
        return Ok(ReleaseCommand::Evidence(paths));
    }
    let mut values = parse_named_values(&args[1..])?;
    let install_root = take_path(&mut values, "--root", true)?;
    let repository = take(&mut values, "--repo")?;
    let workflow = take(&mut values, "--workflow")?;
    let source_ref = take(&mut values, "--source-ref")?;
    ReleaseTrustPolicy::github_at_ref(&repository, &workflow, &source_ref)
        .map_err(|_| "invalid release trust policy".to_owned())?;
    let gh_program = values
        .remove("--gh")
        .map_or_else(|| OsString::from("gh"), OsString::from);
    let common = ReleaseCommonCommand {
        install_root,
        repository,
        workflow,
        source_ref,
        gh_program,
    };
    let command = match operation {
        ReleaseOperation::Apply => {
            let manifest = take_path(&mut values, "--manifest", true)?;
            let manifest_bundle = take_path(&mut values, "--manifest-bundle", true)?;
            let artifact = take_path(&mut values, "--artifact", true)?;
            let artifact_bundle = take_path(&mut values, "--artifact-bundle", true)?;
            let channel = parse_channel(&take(&mut values, "--channel")?)?;
            ReleaseCommand::Apply(ReleaseApplyCommand {
                common,
                manifest,
                manifest_bundle,
                artifact,
                artifact_bundle,
                channel,
            })
        }
        ReleaseOperation::Rollback => ReleaseCommand::Rollback(common),
        ReleaseOperation::Evidence => {
            return Err("release evidence does not accept named options".to_owned());
        }
    };
    if let Some(unknown) = values.keys().next() {
        return Err(format!("unknown release option `{unknown}`\n{}", usage()));
    }
    Ok(command)
}

/// Execute through the minimal sandbox/subprocess/release-manager plugin world.
///
/// The caller supplies the authoritative user Settings path, plugin cache and
/// current config-version classification. No provider/session/TUI world is
/// composed.
///
/// # Errors
/// Plugin-state/cache admission, composition, signature, policy, install, or
/// rollback failures return bounded value-free text.
pub fn run(
    command: &ReleaseCommand,
    settings_user_path: PathBuf,
    plugin_cache_root: PathBuf,
    config: ConfigVersionState,
) -> Result<String, String> {
    if let ReleaseCommand::Evidence(paths) = command {
        return evaluate_evidence(paths);
    }
    let plugins = enabled_plugin_compatibility(&settings_user_path, &plugin_cache_root)?;
    let common = command
        .common()
        .ok_or_else(|| "release command has no manager inputs".to_owned())?;
    let trust =
        ReleaseTrustPolicy::github_at_ref(&common.repository, &common.workflow, &common.source_ref)
            .map_err(|_| "invalid release trust policy".to_owned())?;
    let platform = ReleasePlatform::host().map_err(|error| error.to_string())?;
    let verification_root = tempfile::Builder::new()
        .prefix("heycode-release-verification-")
        .tempdir()
        .map_err(|_| "release verification scratch is unavailable".to_owned())?;
    let manager_config = GhReleaseManagerConfig::new(
        common.install_root.clone(),
        verification_root.path().join("scratch"),
        common.gh_program.clone(),
        platform,
        trust,
    );
    let sandbox = heycode_exec::SandboxService::new(
        heycode_exec::SandboxMode::Off,
        std::env::current_dir().map_err(|_| "release working directory is unavailable")?,
        None,
    )
    .map_err(|_| "release sandbox policy is unavailable".to_owned())?;
    let release_plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![
        heycode_exec::sandbox_service_plugin(sandbox),
        heycode_exec::local_subprocess_plugin(),
        gh_release_manager_plugin(manager_config),
    ];
    let mut context = heycode_core::compose(&release_plugins).map_err(|error| error.to_string())?;
    let Some(manager) = context.get::<ReleaseManager>(SERVICE_RELEASE_MANAGER) else {
        context.shutdown();
        return Err("release manager service is unavailable".to_owned());
    };
    let result = match command {
        ReleaseCommand::Apply(command) => ReleaseBundle::read_local(
            &command.manifest,
            &command.manifest_bundle,
            &command.artifact,
            &command.artifact_bundle,
        )
        .and_then(|bundle| manager.apply(bundle, command.channel.clone(), plugins))
        .map(render_apply)
        .map_err(render_manager_error),
        ReleaseCommand::Rollback(_) => manager
            .rollback(config, plugins)
            .map(render_rollback)
            .map_err(render_manager_error),
        ReleaseCommand::Evidence(_) => {
            Err("release evidence did not exit before manager composition".to_owned())
        }
    };
    context.shutdown();
    result
}

fn evaluate_evidence(paths: &[PathBuf]) -> Result<String, String> {
    let mut observations = Vec::new();
    for path in paths {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|_| "fresh-machine evidence is unavailable".to_owned())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
            return Err("fresh-machine evidence is unavailable".to_owned());
        }
        let bytes =
            std::fs::read(path).map_err(|_| "fresh-machine evidence is unavailable".to_owned())?;
        observations.extend(
            heycode_install::parse_fresh_machine_evidence(&bytes)
                .map_err(|error| error.to_string())?,
        );
    }
    let matrix = heycode_install::FreshMachineMatrix::evaluate(observations)
        .map_err(|error| error.to_string())?;
    if matrix.q16_real_turn_complete() {
        Ok("Q16 real-provider onboarding matrix passed".to_owned())
    } else {
        Err("Q16 real-provider onboarding matrix is incomplete".to_owned())
    }
}

fn enabled_plugin_compatibility(
    settings_user_path: &Path,
    cache_root: &Path,
) -> Result<PluginCompatibilitySet, String> {
    let (mut context, lifecycle) = crate::plugin_cli::compose_lifecycle_world(
        settings_user_path.to_path_buf(),
        cache_root.to_path_buf(),
    )
    .map_err(|_| "enabled plugin snapshot is unavailable".to_owned())?;
    let result = (|| {
        let states = lifecycle
            .list()
            .map_err(|_| "enabled plugin snapshot is unavailable".to_owned())?;
        let enabled = states
            .into_iter()
            .filter(|state| state.enabled)
            .collect::<Vec<_>>();
        if enabled.is_empty() {
            return Ok(PluginCompatibilitySet::empty());
        }
        let api = heycode_extensions::ApiVersion::new(heycode_extensions::PLUGIN_API_VERSION)
            .map_err(|_| "plugin host API is invalid".to_owned())?;
        let validator =
            heycode_extensions::ManifestValidator::new(api, crate::plugin_cli::host_platform());
        let cache = heycode_extensions::PluginInstallCache::open(cache_root, validator)
            .map_err(|_| "enabled plugin cache is unavailable".to_owned())?;
        let mut rows = Vec::with_capacity(enabled.len());
        for state in enabled {
            let installed = cache
                .resolve(&state.id, &state.active)
                .map_err(|_| "enabled plugin package is unavailable".to_owned())?;
            let range = installed.manifest().api();
            rows.push(
                PluginApiCompatibility::new(
                    state.id.as_str(),
                    range.minimum.get(),
                    range.maximum.get(),
                )
                .map_err(|_| "enabled plugin API range is invalid".to_owned())?,
            );
        }
        PluginCompatibilitySet::new(rows)
            .map_err(|_| "enabled plugin snapshot is invalid".to_owned())
    })();
    context.shutdown();
    result
}

fn parse_named_values(args: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let name = args
            .get(index)
            .filter(|value| value.starts_with("--"))
            .ok_or_else(usage)?
            .clone();
        index += 1;
        let value = args
            .get(index)
            .ok_or_else(|| format!("{name} needs a value"))?;
        if values.insert(name.clone(), value.clone()).is_some() {
            return Err(format!("release option `{name}` may be supplied only once"));
        }
        index += 1;
    }
    Ok(values)
}

fn take(values: &mut BTreeMap<String, String>, name: &str) -> Result<String, String> {
    values
        .remove(name)
        .ok_or_else(|| format!("release operation requires {name}"))
}

fn take_path(
    values: &mut BTreeMap<String, String>,
    name: &str,
    absolute: bool,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(take(values, name)?);
    if absolute && !path.is_absolute() {
        return Err(format!("{name} must be an absolute path"));
    }
    Ok(path)
}

fn parse_channel(value: &str) -> Result<ReleaseChannel, String> {
    match value {
        "stable" => Ok(ReleaseChannel::Stable),
        "preview" => Ok(ReleaseChannel::Preview),
        _ => value
            .strip_prefix("pinned:")
            .ok_or_else(|| {
                "release channel must be stable, preview, or pinned:<version>".to_owned()
            })
            .and_then(|version| {
                ReleaseVersion::parse(version)
                    .map(ReleaseChannel::Pinned)
                    .map_err(|_| "release pin is invalid".to_owned())
            }),
    }
}

fn render_apply(outcome: ReleaseApplyOutcome) -> String {
    match outcome {
        ReleaseApplyOutcome::Installed(outcome) => format!(
            "{} heycode {}",
            match outcome.disposition {
                InstallDisposition::FreshInstall => "installed",
                InstallDisposition::Update => "updated",
            },
            outcome.version
        ),
        ReleaseApplyOutcome::Current(version) => format!("heycode {version} is already current"),
        _ => "release apply outcome is unsupported".to_owned(),
    }
}

fn render_rollback(outcome: RollbackOutcome) -> String {
    match outcome {
        RollbackOutcome::RolledBack { to } => format!("rolled heycode back to {to}"),
        RollbackOutcome::Refused(_) => "release rollback was refused".to_owned(),
        _ => "release rollback outcome is unsupported".to_owned(),
    }
}

fn render_manager_error(error: ReleaseManagerError) -> String {
    error.to_string()
}
