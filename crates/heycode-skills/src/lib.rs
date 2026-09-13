//! heycode-skills — SKILL.md capability packs (the Claude Code model).
//!
//! Discovery scans rank-ordered roots, first hit wins a name:
//! 1. `<project>/.heycode/skills`
//! 2. `<project>/.agents/skills`
//! 3. `$HEYCODE_HOME/skills`
//!
//! A skill is a directory containing `SKILL.md` with YAML-ish frontmatter
//! (`name`, `description`, optional `disable-model-invocation: true`) followed
//! by the full markdown body. Model-invocable skills appear in a prompt
//! catalog section; the `load_skill` tool returns the full body; the human
//! `/skill <name> [prompt]` command injects any enabled skill — including
//! frontmatter user-invocation-only ones — as the next turn's context.
//!
//! Discovery never resolves an ambient skill path. Callers bind each root to
//! an authorized directory capability, and discovery opens every relative
//! directory and `SKILL.md` with no-follow semantics. Skill bodies are then
//! immutable in-memory snapshots for both model and human loading.

mod doctor;
pub use doctor::{SkillDoctorRow, SkillDoctorSnapshot};
mod preferences;
mod reload;

pub use preferences::{
    SkillAdmission, SkillCatalogRecord, SkillCatalogSnapshot, SkillPreferenceError, SkillSort,
    settings_definition, settings_namespace,
};
pub use reload::{
    PinnedSkillReloadGuard, SkillReloadError, SkillReloadGuard, SkillReloadOutcome,
    WorkspaceSkillReloadGuard,
};

use std::ffi::OsString;
use std::io::{self, Read as _};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use cap_fs_ext::{DirExt as _, FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions};
use cap_std::time::SystemTime;

// reserved for command impls below
use heycode_agent::{Command, CommandRegistry};
use heycode_core::{CoreError, CoreResult, Plugin, ToolSpec};
use heycode_prompt::PromptRegistry;
use heycode_tools::{Tool, ToolCtx, ToolError, ToolRegistry};

/// Discovered skill-set service.
pub const SERVICE_SKILLS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("skills");

const MAX_SKILL_BYTES: u64 = 1024 * 1024;

/// One discovery root bound beneath an already-opened authority directory.
///
/// The authority handle fixes directory identity for the lifetime of this
/// value. `relative` is traversed one normal component at a time without
/// following symlinks.
#[derive(Clone)]
pub struct SkillRoot {
    authority: Arc<Dir>,
    relative: PathBuf,
    scope: SkillSourceScope,
}

impl SkillRoot {
    /// Bind a relative skill root beneath an existing authorized directory.
    ///
    /// The nearest existing authority ancestor is canonicalized and reopened
    /// component-by-component. Any absent authority suffix joins the untrusted
    /// relative path, which is later traversed without symlinks. Binding never
    /// creates a directory.
    ///
    /// # Errors
    /// Relative authorities, non-directory existing ancestors,
    /// empty/absolute/unsafe relative paths, or a filesystem race while
    /// opening the authority are rejected.
    pub fn bind(
        authority: impl AsRef<Path>,
        relative: impl AsRef<Path>,
    ) -> Result<Self, io::Error> {
        let requested_relative = validate_relative_root(relative.as_ref())?;
        let (authority, missing_suffix) = open_authority(authority.as_ref())?;
        let relative = validate_relative_root(&missing_suffix.join(requested_relative))?;
        Ok(Self {
            authority: Arc::new(authority),
            relative,
            scope: SkillSourceScope::Configured,
        })
    }

    fn from_authority(
        authority: Arc<Dir>,
        relative: impl AsRef<Path>,
        scope: SkillSourceScope,
    ) -> Result<Self, io::Error> {
        Ok(Self {
            authority,
            relative: validate_relative_root(relative.as_ref())?,
            scope,
        })
    }

    fn open(&self) -> Result<Dir, io::Error> {
        open_relative_dir(&self.authority, &self.relative)
    }
}

/// One discovered skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Frontmatter name (falls back to directory name).
    pub name: String,
    /// One-line purpose shown in the catalog.
    pub description: String,
    /// When true, only `/skill` may load it; the tool refuses.
    pub disable_model_invocation: bool,
    /// Full markdown body after frontmatter.
    pub body: String,
}

/// Trust scope that admitted a discovered skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSourceScope {
    /// A project directory explicitly admitted by workspace trust.
    Project,
    /// The user's configured heycode home.
    User,
    /// A caller-bound root whose scope is not otherwise classified.
    Configured,
    /// A live declarative/plugin contribution rather than a filesystem scan.
    Contribution,
}

impl SkillSourceScope {
    /// Stable source-scope name for diagnostics and UI attribution.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
            Self::Configured => "configured",
            Self::Contribution => "contribution",
        }
    }
}

/// Source attribution for one admitted skill snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSource {
    scope: SkillSourceScope,
    root: String,
    directory: String,
}

impl SkillSource {
    fn discovered(scope: SkillSourceScope, root: String, directory: String) -> Self {
        Self {
            scope,
            root,
            directory,
        }
    }

    fn contribution() -> Self {
        Self {
            scope: SkillSourceScope::Contribution,
            root: "live registry".to_owned(),
            directory: String::new(),
        }
    }

    /// Authority class that admitted this row.
    #[must_use]
    pub const fn scope(&self) -> SkillSourceScope {
        self.scope
    }

    /// Safe configured root label; never an ambient canonical path.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Directory name beneath `root`, empty for live contributions.
    #[must_use]
    pub fn directory(&self) -> &str {
        &self.directory
    }
}

/// One skill plus the authority source that admitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRecord {
    /// Immutable instruction snapshot.
    pub skill: Skill,
    /// Source attribution captured in the same registry generation.
    pub source: SkillSource,
}

impl Skill {
    /// Split frontmatter + body with a minimal `key: value` parser.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let rest = raw.trim_start().strip_prefix("---")?;
        let (front, body) = rest.split_once("---")?;
        let mut name = None;
        let mut description = String::new();
        let mut disable = false;
        for line in front.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim().trim_matches('"').trim_matches('\'').to_owned();
            match key.trim() {
                "name" => name = Some(value),
                "description" => description = value,
                "disable-model-invocation" => {
                    disable = matches!(value.as_str(), "true" | "yes" | "1");
                }
                _ => {}
            }
        }
        Some(Self {
            name: name.unwrap_or_default(),
            description,
            disable_model_invocation: disable,
            body: body.trim_start().to_owned(),
        })
    }
}

/// Discover skills across roots; FIRST root that has a name wins.
#[must_use]
pub fn discover(roots: &[SkillRoot]) -> Vec<Skill> {
    discover_report(roots).skills
}

/// One skill directory discovery could not admit, and why.
///
/// A malformed `SKILL.md` is the user's data, so it is reported here for the
/// `/skills` panel rather than aborting composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedSkill {
    /// Root the directory was found under, as a display path.
    pub root: String,
    /// Directory name under that root.
    pub directory: String,
    /// Static reason safe to render.
    pub reason: String,
}

/// Everything discovery found: the admitted skills and the rows it skipped.
#[derive(Debug, Clone, Default)]
pub struct SkillDiscovery {
    /// Skills whose names are valid and unique, first root wins.
    pub skills: Vec<Skill>,
    /// Source for each admitted skill at the same index as `skills`.
    pub sources: Vec<SkillSource>,
    /// Directories that were skipped, in discovery order.
    pub skipped: Vec<SkippedSkill>,
    /// Root-level failures that make a live rescan unsafe to publish.
    pub issues: Vec<SkillDiscoveryIssue>,
}

/// One root could not be scanned as one stable no-follow snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiscoveryIssue {
    /// Safe configured label, not an ambient canonical path.
    pub root: String,
    /// Fixed diagnostic category with no operating-system details.
    pub reason: String,
}

/// Discover skills and report what was skipped.
///
/// Every admitted skill already satisfies [`SkillSet::new`]'s name rules, so a
/// registry seeded from this report cannot fail on user data.
#[must_use]
pub fn discover_report(roots: &[SkillRoot]) -> SkillDiscovery {
    let mut report = SkillDiscovery::default();
    let mut seen: Vec<String> = Vec::new();
    for root in roots {
        let root_label = root.relative.display().to_string();
        let candidates = match discover_root(root) {
            Ok(candidates) => candidates,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                report.issues.push(SkillDiscoveryIssue {
                    root: root_label,
                    reason: "root was unreadable or changed during discovery".to_owned(),
                });
                continue;
            }
        };
        for (directory_name, mut skill) in candidates {
            let directory = directory_name.to_string_lossy().into_owned();
            if skill.name.is_empty() {
                skill.name = directory.clone();
            }
            if validate_skill_name(&skill.name).is_err() {
                report.skipped.push(SkippedSkill {
                    root: root_label.clone(),
                    directory,
                    reason: "invalid skill name: non-empty, at most 128 characters, no whitespace or control characters".to_owned(),
                });
                continue;
            }
            if seen.contains(&skill.name) {
                // A later root repeating a name is the documented "first root
                // wins" rule, not a defect worth a row.
                continue;
            }
            seen.push(skill.name.clone());
            report.sources.push(SkillSource::discovered(
                root.scope,
                root_label.clone(),
                directory.clone(),
            ));
            report.skills.push(skill);
        }
    }
    report
}

fn discover_root(root: &SkillRoot) -> Result<Vec<(OsString, Skill)>, io::Error> {
    let directory = root.open()?;
    let before = DirectoryStamp::of(&directory)?;
    let mut entries = directory.entries()?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(cap_std::fs::DirEntry::file_name);
    let mut skills = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        let Ok(skill_directory) = directory.open_dir_nofollow(&name) else {
            continue;
        };
        let Ok(directory_before) = DirectoryStamp::of(&skill_directory) else {
            continue;
        };
        let Some(raw) = read_skill_document(&skill_directory) else {
            continue;
        };
        let Ok(directory_after) = DirectoryStamp::of(&skill_directory) else {
            continue;
        };
        let Ok(current_directory) = directory.open_dir_nofollow(&name) else {
            continue;
        };
        let Ok(current_identity) = DirectoryStamp::of(&current_directory) else {
            continue;
        };
        if directory_before != directory_after || !directory_before.same_identity(current_identity)
        {
            continue;
        }
        let skill = Skill::parse(&raw).unwrap_or_else(|| Skill {
            name: name.to_string_lossy().into_owned(),
            description: String::new(),
            disable_model_invocation: false,
            body: raw,
        });
        skills.push((name, skill));
    }
    let after = DirectoryStamp::of(&directory)?;
    let current_root = root.open()?;
    let current_identity = DirectoryStamp::of(&current_root)?;
    if before != after || !before.same_identity(current_identity) {
        return Err(io::Error::other("skill root changed during discovery"));
    }
    Ok(skills)
}

fn read_skill_document(directory: &Dir) -> Option<String> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = directory.open_with("SKILL.md", &options).ok()?;
    let before = FileStamp::of(&file).ok()?;
    if !before.regular || before.links != 1 || before.length > MAX_SKILL_BYTES {
        return None;
    }
    let capacity = usize::try_from(before.length).ok()?;
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(MAX_SKILL_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).ok()? != before.length {
        return None;
    }
    let after = FileStamp::of(&file).ok()?;
    let current = directory.open_with("SKILL.md", &options).ok()?;
    let current = FileStamp::of(&current).ok()?;
    if before != after || before != current {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectoryStamp {
    device: u64,
    inode: u64,
    modified: Option<SystemTime>,
}

impl DirectoryStamp {
    fn of(directory: &Dir) -> Result<Self, io::Error> {
        let metadata = directory.dir_metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::other("opened skill root is not a directory"));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            modified: metadata.modified().ok(),
        })
    }

    const fn same_identity(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: Option<SystemTime>,
    links: u64,
    regular: bool,
}

impl FileStamp {
    fn of(file: &cap_std::fs::File) -> Result<Self, io::Error> {
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: metadata.modified().ok(),
            links: metadata.nlink(),
            regular: metadata.is_file(),
        })
    }
}

fn validate_relative_root(path: &Path) -> Result<PathBuf, io::Error> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "skill root must be a non-empty normal relative path",
        ));
    }
    Ok(path.to_path_buf())
}

fn open_authority(path: &Path) -> Result<(Dir, PathBuf), io::Error> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "skill authority must be an absolute path without traversal",
        ));
    }
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    let canonical = loop {
        match std::fs::canonicalize(&existing) {
            Ok(canonical) => break canonical,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "skill authority has no existing ancestor",
                    )
                })?;
                missing.push(name.to_os_string());
                existing = existing.parent().map(Path::to_path_buf).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "skill authority has no existing ancestor",
                    )
                })?;
            }
            Err(error) => return Err(error),
        }
    };
    let directory = open_absolute_dir_nofollow(&canonical)?;
    let identity = DirectoryStamp::of(&directory)?;
    let confirmed = std::fs::canonicalize(&existing)?;
    if canonical != confirmed {
        return Err(io::Error::other(
            "skill authority changed while it was opened",
        ));
    }
    let reopened = open_absolute_dir_nofollow(&confirmed)?;
    if !identity.same_identity(DirectoryStamp::of(&reopened)?) {
        return Err(io::Error::other(
            "skill authority changed while it was opened",
        ));
    }
    let mut missing_suffix = PathBuf::new();
    for component in missing.into_iter().rev() {
        missing_suffix.push(component);
    }
    Ok((directory, missing_suffix))
}

fn open_absolute_dir_nofollow(path: &Path) -> Result<Dir, io::Error> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "skill authority must resolve to an absolute path",
        ));
    }
    let mut volume_root = PathBuf::new();
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => volume_root.push(prefix.as_os_str()),
            Component::RootDir => volume_root.push(component.as_os_str()),
            Component::Normal(name) => relative.push(name),
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "canonical skill authority contains traversal",
                ));
            }
        }
    }
    let root = Dir::open_ambient_dir(&volume_root, cap_std::ambient_authority())?;
    open_relative_dir(&root, &relative)
}

fn open_relative_dir(authority: &Dir, relative: &Path) -> Result<Dir, io::Error> {
    let mut directory = authority.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "skill root contains traversal",
            ));
        };
        directory = directory.open_dir_nofollow(name)?;
    }
    Ok(directory)
}

/// The model-facing loader: refuses user-invocation-only skills so the
/// restriction is enforced at THE dispatch point, not by convention.
pub struct LoadSkillTool {
    skills: SkillSet,
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "load_skill".to_owned(),
            description: "Load the complete instructions for an available skill \
                          before following it. Only skills listed in your catalog \
                          are loadable here."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["name"],
                "properties": {
                    "name": {"type": "string", "description": "Skill name from the catalog"}
                }
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        _cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let wanted = args
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("`name` must be a string"))?;
        let found = self
            .skills
            .get_model_invocable(wanted)
            .map_err(|error| ToolError::new(error.to_string()))?;
        let Some(skill) = found else {
            return Err(ToolError::new(format!(
                "unknown skill `{wanted}` — see the skills catalog"
            )));
        };
        Ok(serde_json::Value::String(format!(
            "<skill name=\"{}\">\n{}\n</skill>",
            skill.name, skill.body
        )))
    }
}

/// Mount the skills capability: catalog section + loader tool.
///
/// Injects: `tools`, `prompt`.
pub fn skills_plugin(roots: Vec<SkillRoot>) -> Box<dyn Plugin> {
    skills_plugin_with_reload_guard(roots, Arc::new(PinnedSkillReloadGuard))
}

/// Mount project skills with a reload guard resolved from the composed
/// workspace-transition owner.
///
/// # Errors
/// The expected project directory must already be a stable existing directory.
pub fn skills_plugin_with_workspace_reload_guard(
    roots: Vec<SkillRoot>,
    expected_project: impl AsRef<Path>,
) -> Result<Box<dyn Plugin>, SkillReloadError> {
    Ok(skills_plugin_with_reload_guard(
        roots,
        Arc::new(WorkspaceSkillReloadGuard::deferred(expected_project)?),
    ))
}

/// Mount skills with an explicit dynamic-workspace rescan guard.
/// Guard failures are reported by `/reload-skills`; initial composition still
/// uses only the already-bound no-follow roots.
pub fn skills_plugin_with_reload_guard(
    roots: Vec<SkillRoot>,
    reload_guard: Arc<dyn SkillReloadGuard>,
) -> Box<dyn Plugin> {
    struct SkillsPlugin {
        roots: Vec<SkillRoot>,
        reload_guard: Arc<dyn SkillReloadGuard>,
    }
    impl Plugin for SkillsPlugin {
        fn name(&self) -> &'static str {
            "skills"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "skills",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Tool,
                    heycode_core::PluginContributionKind::PromptSection,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            [
                (heycode_core::ContributionKind::Tool, "load_skill"),
                (
                    heycode_core::ContributionKind::PromptSection,
                    "skills-catalog",
                ),
                (heycode_core::ContributionKind::Command, "skills"),
                (heycode_core::ContributionKind::Command, "skill"),
                (heycode_core::ContributionKind::Command, "skill-doctor"),
                (heycode_core::ContributionKind::Command, "reload-skills"),
                (
                    heycode_core::ContributionKind::SettingsNamespace,
                    "skills-preferences",
                ),
            ]
            .into_iter()
            .map(|(kind, name)| heycode_core::PluginContributionSpec::new(kind, name))
            .collect()
        }
        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SKILLS]
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            const BASE: &[heycode_core::ServiceKey] = &[
                heycode_tools::SERVICE_TOOLS,
                heycode_prompt::SERVICE_PROMPT,
                heycode_agent::SERVICE_COMMANDS,
                heycode_settings::SERVICE_SETTINGS,
            ];
            const WITH_WORKSPACE: &[heycode_core::ServiceKey] = &[
                heycode_tools::SERVICE_TOOLS,
                heycode_prompt::SERVICE_PROMPT,
                heycode_agent::SERVICE_COMMANDS,
                heycode_settings::SERVICE_SETTINGS,
                heycode_agent::workspace_transition::SERVICE_WORKSPACE_TRANSITION,
            ];
            if self.reload_guard.requires_workspace() {
                WITH_WORKSPACE
            } else {
                BASE
            }
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> CoreResult<()> {
            // Discovery is eager at load and fail-soft on user data: unreadable
            // roots and malformed skills are skipped rows, never a refusal to
            // start.
            self.reload_guard.bind_context(ctx)?;
            let settings = ctx
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings missing"))?;
            let preferences_namespace =
                settings_namespace().map_err(|error| CoreError::other(error.to_string()))?;
            let settings_snapshot = settings
                .register(
                    ctx,
                    settings_definition().map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let preference_state = preferences::parse_snapshot(settings_snapshot)
                .map_err(|error| CoreError::other(error.to_string()))?;
            let preference_state = Arc::new(std::sync::RwLock::new(preference_state));
            let skills = SkillSet::from_discovery_with_reload_and_preferences(
                discover_report(&self.roots),
                self.roots.clone(),
                self.reload_guard.clone(),
                preference_state,
                Some(preferences::PreferencesBinding {
                    settings: settings.clone(),
                    namespace: preferences_namespace.clone(),
                }),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let watched_skills = skills.clone();
            settings
                .watch(ctx, &preferences_namespace, move |change| {
                    let _ = watched_skills.apply_preference_snapshot(change.next().clone());
                })
                .map_err(|error| CoreError::other(error.to_string()))?;
            let tools = ctx
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tools missing"))?;
            let prompt = ctx
                .get::<PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
                .ok_or_else(|| CoreError::other("prompt missing"))?;
            let commands = ctx
                .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("commands missing"))?;

            tools
                .register_shared(Arc::new(LoadSkillTool {
                    skills: skills.clone(),
                }))
                .map_err(|e| CoreError::other(e.to_string()))?;

            let snapshot = skills.clone();
            prompt
                .section_shared("skills-catalog", 90, move |_cx| {
                    let Ok(skills) = snapshot.model_invocable_snapshot() else {
                        return "# Available skills\nSkill registry unavailable.".to_owned();
                    };
                    let invocable: Vec<String> = skills
                        .iter()
                        .map(|s| {
                            if s.description.is_empty() {
                                format!("- {}", s.name)
                            } else {
                                format!("- {}: {}", s.name, s.description)
                            }
                        })
                        .collect();
                    if invocable.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "# Available skills\nCall `load_skill` to read one before following it.\n{}",
                            invocable.join("\n")
                        )
                    }
                })
                .map_err(CoreError::other)?;

            // Publish for /skills and /skill commands.
            struct SkillsList {
                descriptor: heycode_agent::CommandDescriptor,
                panel: heycode_agent::UiPanelId,
            }
            #[async_trait]
            impl Command for SkillsList {
                fn descriptor(&self) -> &heycode_agent::CommandDescriptor {
                    &self.descriptor
                }
                async fn execute(
                    &self,
                    agent: &heycode_agent::Agent,
                    args: &str,
                ) -> anyhow::Result<()> {
                    if !args.trim().is_empty() {
                        anyhow::bail!("usage: /skills");
                    }
                    agent
                        .ui()
                        .emit(heycode_agent::UiEvent::CapabilityPanelRequested {
                            panel: self.panel.clone(),
                        });
                    Ok(())
                }
            }
            struct SkillInvoke {
                skills: SkillSet,
                descriptor: heycode_agent::CommandDescriptor,
            }
            #[async_trait]
            impl Command for SkillInvoke {
                fn descriptor(&self) -> &heycode_agent::CommandDescriptor {
                    &self.descriptor
                }
                fn availability(&self) -> heycode_agent::CommandAvailability {
                    match self.skills.catalog_snapshot() {
                        Ok(catalog) if !catalog.records().iter().any(|row| row.enabled()) => {
                            heycode_agent::CommandAvailability::unavailable(
                                "No skills were discovered",
                            )
                            .unwrap_or_else(|_| heycode_agent::CommandAvailability::available())
                        }
                        Ok(_) => heycode_agent::CommandAvailability::available(),
                        Err(_) => heycode_agent::CommandAvailability::unavailable(
                            "Skill registry is unavailable",
                        )
                        .unwrap_or_else(|_| heycode_agent::CommandAvailability::available()),
                    }
                }
                async fn execute(
                    &self,
                    agent: &heycode_agent::Agent,
                    args: &str,
                ) -> anyhow::Result<()> {
                    let mut parts = args.splitn(2, char::is_whitespace);
                    let name = parts.next().unwrap_or_default().trim();
                    let rest = parts.next().unwrap_or_default().trim();
                    let Some(sk) = self
                        .skills
                        .get_enabled(name)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?
                    else {
                        anyhow::bail!("unknown skill `{name}` — try /skills");
                    };
                    let composed = format!(
                        "<skill name=\"{}\">\n{}\n</skill>\n\n{}",
                        sk.name,
                        sk.body,
                        if rest.is_empty() {
                            "Follow this skill."
                        } else {
                            rest
                        }
                    );
                    let report = agent.send(&composed).await?;
                    let _ = report;
                    Ok(())
                }
            }
            let source = heycode_agent::CommandSource::from_plugin("skills")
                .map_err(|error| CoreError::other(error.to_string()))?;
            let list_descriptor = heycode_agent::CommandDescriptor::new(
                "skills",
                "Open discovered skills",
                Vec::new(),
                heycode_agent::CommandTiming::Immediate,
                source.clone(),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let name_argument =
                heycode_agent::CommandArgument::required("name", "Discovered skill name")
                    .map_err(|error| CoreError::other(error.to_string()))?;
            let prompt_argument =
                heycode_agent::CommandArgument::optional("prompt", "Optional task prompt")
                    .map_err(|error| CoreError::other(error.to_string()))?
                    .variadic();
            let invoke_descriptor = heycode_agent::CommandDescriptor::new(
                "skill",
                "Run a model turn with one skill loaded",
                vec![name_argument, prompt_argument],
                heycode_agent::CommandTiming::ModelScheduling,
                source,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(SkillsList {
                        descriptor: list_descriptor,
                        panel: heycode_agent::UiPanelId::new("skills")
                            .map_err(|error| CoreError::other(error.to_string()))?,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    ctx,
                    Arc::new(SkillInvoke {
                        skills: skills.clone(),
                        descriptor: invoke_descriptor,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            doctor::register(ctx, &commands, skills.clone())?;
            reload::register(ctx, &commands, skills.clone())?;
            ctx.provide(SERVICE_SKILLS, "skills", skills)
        }
    }
    Box::new(SkillsPlugin {
        roots,
        reload_guard,
    })
}

/// Dynamic, effect-ownable skill registry published as the `"skills"` service.
///
/// Discovery seeds the registry once, while declarative plugins may add and
/// withdraw exact rows. Reads always clone an immutable snapshot so a prompt
/// render or tool dispatch cannot observe a half-applied generation.
#[derive(Clone)]
pub struct SkillSet {
    inner: Arc<Mutex<SkillState>>,
    roots: Arc<Vec<SkillRoot>>,
    reload_guard: Arc<dyn reload::SkillReloadGuard>,
    preferences: Arc<std::sync::RwLock<preferences::PreferenceState>>,
    preferences_binding: Option<preferences::PreferencesBinding>,
}

struct SkillState {
    entries: Vec<SkillEntry>,
    skipped: Vec<SkippedSkill>,
    revision: u64,
    reload_diagnostics: Vec<String>,
}

#[derive(Clone)]
struct SkillEntry {
    record: SkillRecord,
    preference_aliases: Vec<String>,
    token: Arc<()>,
    discovered: bool,
}

/// A late skill registration. Dropping it removes exactly the row it owns.
pub struct SkillRegistration {
    inner: Weak<Mutex<SkillState>>,
    name: String,
    token: Arc<()>,
    active: bool,
}

/// Skill registry failures exposed without document bodies.
#[derive(Debug, thiserror::Error)]
pub enum SkillRegistryError {
    /// The supplied skill has no stable safe name.
    #[error("skill name is invalid")]
    InvalidName,
    /// Another live row owns the same name.
    #[error("skill `{name}` is already registered")]
    Duplicate {
        /// Contested safe name.
        name: String,
    },
    /// Persisted preferences exclude this row from every invocation path.
    #[error("skill `{name}` is disabled — open /skills to enable it")]
    Disabled {
        /// Canonical safe name.
        name: String,
    },
    /// Persisted preferences or source metadata allow only explicit `/skill`.
    #[error("skill `{name}` is user-invoked only — ask the person to run /skill {name}")]
    UserOnly {
        /// Canonical safe name.
        name: String,
    },
    /// Registry synchronization failed.
    #[error("skill registry is unavailable")]
    Unavailable,
}

impl SkillSet {
    /// Seed a registry from already-discovered skills.
    ///
    /// # Errors
    /// Invalid or duplicate names fail before publication.
    pub fn new(skills: Vec<Skill>) -> Result<Self, SkillRegistryError> {
        let mut entries = Vec::with_capacity(skills.len());
        for skill in skills {
            validate_skill_name(&skill.name)?;
            if entries
                .iter()
                .any(|row: &SkillEntry| row.record.skill.name == skill.name)
            {
                return Err(SkillRegistryError::Duplicate {
                    name: skill.name.clone(),
                });
            }
            entries.push(SkillEntry {
                record: SkillRecord {
                    skill,
                    source: SkillSource::contribution(),
                },
                preference_aliases: Vec::new(),
                token: Arc::new(()),
                discovered: false,
            });
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(SkillState {
                entries,
                skipped: Vec::new(),
                revision: 1,
                reload_diagnostics: Vec::new(),
            })),
            roots: Arc::new(Vec::new()),
            reload_guard: Arc::new(reload::PinnedSkillReloadGuard),
            preferences: preferences::detached_preferences(),
            preferences_binding: None,
        })
    }

    /// Seed a registry from a discovery report, retaining its skipped rows.
    ///
    /// # Errors
    /// Duplicate names fail before publication; invalid names cannot occur
    /// because discovery already skipped them.
    pub fn from_discovery(discovery: SkillDiscovery) -> Result<Self, SkillRegistryError> {
        Self::from_discovery_with_reload(
            discovery,
            Vec::new(),
            Arc::new(reload::PinnedSkillReloadGuard),
        )
    }

    /// Directories discovery could not admit, for the `/skills` panel.
    #[must_use]
    pub fn skipped(&self) -> Vec<SkippedSkill> {
        self.inner
            .lock()
            .map(|state| state.skipped.clone())
            .unwrap_or_else(|_| {
                vec![SkippedSkill {
                    root: "registry".to_owned(),
                    directory: String::new(),
                    reason: "skill registry is unavailable".to_owned(),
                }]
            })
    }

    /// Safe diagnostics from the latest refused reload attempt.
    #[must_use]
    pub fn reload_diagnostics(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|state| state.reload_diagnostics.clone())
            .unwrap_or_else(|_| vec!["skill registry is unavailable".to_owned()])
    }

    /// Current successful registry generation.
    pub fn generation(&self) -> Result<u64, SkillRegistryError> {
        self.inner
            .lock()
            .map(|state| state.revision)
            .map_err(|_| SkillRegistryError::Unavailable)
    }

    /// Source-attributed catalog plus exact mutation generations.
    ///
    /// # Errors
    /// Poisoned registry or preference state fails loud.
    pub fn catalog_snapshot(&self) -> Result<SkillCatalogSnapshot, SkillRegistryError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        let preferences = self
            .preferences
            .read()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        let mut records = active_rows(&state.entries)
            .into_iter()
            .map(|row| {
                SkillCatalogRecord::new(
                    row.record.clone(),
                    row_admission(row, &preferences.admission_overrides),
                )
            })
            .collect::<Vec<_>>();
        match preferences.sort {
            SkillSort::Name => records.sort_by(|left, right| {
                left.record()
                    .skill
                    .name
                    .to_lowercase()
                    .cmp(&right.record().skill.name.to_lowercase())
                    .then_with(|| left.record().skill.name.cmp(&right.record().skill.name))
            }),
            SkillSort::Tokens => records.sort_by(|left, right| {
                right
                    .estimated_catalog_tokens()
                    .cmp(&left.estimated_catalog_tokens())
                    .then_with(|| {
                        left.record()
                            .skill
                            .name
                            .to_lowercase()
                            .cmp(&right.record().skill.name.to_lowercase())
                    })
                    .then_with(|| left.record().skill.name.cmp(&right.record().skill.name))
            }),
            SkillSort::Source => records.sort_by(|left, right| {
                source_sort_key(left.record())
                    .cmp(&source_sort_key(right.record()))
                    .then_with(|| left.record().skill.name.cmp(&right.record().skill.name))
            }),
        }
        Ok(SkillCatalogSnapshot {
            records,
            generation: state.revision,
            sort: preferences.sort,
            admission_overrides: preferences.admission_overrides.clone(),
            settings_snapshot: preferences.settings_snapshot.clone(),
        })
    }

    /// Persist one selected skill's exact four-state admission.
    ///
    /// Source-declared user-only skills cannot be widened to model admission.
    /// Persistence commits before the watcher publishes admission.
    ///
    /// # Errors
    /// Stale rows, a source-policy widening, unavailable persistence, managed
    /// policy, validation, or provider failure leave the prior state active.
    pub fn set_admission(
        &self,
        name: &str,
        admission: SkillAdmission,
        expected: &SkillCatalogSnapshot,
    ) -> Result<SkillCatalogSnapshot, SkillPreferenceError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| SkillPreferenceError::Unavailable)?;
        if state.revision != expected.generation {
            return Err(SkillPreferenceError::StaleGeneration);
        }
        let row = active_row_for_identity(&state.entries, name).ok_or_else(|| {
            SkillPreferenceError::UnknownSkill {
                name: name.to_owned(),
            }
        })?;
        if row.record.skill.disable_model_invocation && admission.model_invocable() {
            return Err(SkillPreferenceError::SourceRestricted {
                name: row.record.skill.name.clone(),
            });
        }
        let binding = self
            .preferences_binding
            .as_ref()
            .ok_or(SkillPreferenceError::PersistenceUnavailable)?;
        let settings_snapshot = expected
            .settings_snapshot
            .as_ref()
            .ok_or(SkillPreferenceError::PersistenceUnavailable)?;
        let canonical = row.record.skill.name.clone();
        let aliases = row.preference_aliases.clone();
        let default_admission = default_admission(&row.record.skill);
        let mut admission_overrides = expected.admission_overrides.clone();
        admission_overrides.remove_identities(
            std::iter::once(canonical.as_str()).chain(aliases.iter().map(String::as_str)),
        );
        if admission != default_admission {
            admission_overrides.insert(canonical, admission);
        }
        let desired_admission = admission_overrides.clone();
        let desired_sort = expected.sort;
        let namespace = binding.namespace.clone();
        let validation_namespace = namespace.clone();
        let next = binding
            .settings
            .replace_user_automatically(
                &namespace,
                preferences::user_value(&admission_overrides, expected.sort),
                settings_snapshot,
                move |candidate| {
                    let (candidate_admission, candidate_sort) =
                        preferences::parse_value(candidate.resolved()).map_err(|message| {
                            heycode_settings::SettingsError::InvalidResolved {
                                namespace: validation_namespace.to_string(),
                                message,
                            }
                        })?;
                    if candidate_admission != desired_admission || candidate_sort != desired_sort {
                        return Err(heycode_settings::SettingsError::InvalidResolved {
                            namespace: validation_namespace.to_string(),
                            message: "a higher-priority setting controls skill preferences"
                                .to_owned(),
                        });
                    }
                    Ok(())
                },
            )
            .map_err(skill_settings_error)?;
        drop(state);
        self.apply_preference_snapshot(next)?;
        self.catalog_snapshot()
            .map_err(|_| SkillPreferenceError::Unavailable)
    }

    /// Persist one selected skill's effective admission state using both the
    /// exact registry generation and exact Settings snapshot captured by the
    /// caller. Persistence commits before the watcher publishes admission.
    ///
    /// # Errors
    /// Stale rows, unavailable persistence, managed policy, validation, or
    /// provider failure leave the prior preference set active.
    pub fn set_enabled(
        &self,
        name: &str,
        enabled: bool,
        expected: &SkillCatalogSnapshot,
    ) -> Result<SkillCatalogSnapshot, SkillPreferenceError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| SkillPreferenceError::Unavailable)?;
        if state.revision != expected.generation {
            return Err(SkillPreferenceError::StaleGeneration);
        }
        let row = active_row_for_identity(&state.entries, name).ok_or_else(|| {
            SkillPreferenceError::UnknownSkill {
                name: name.to_owned(),
            }
        })?;
        let admission = if enabled {
            default_admission(&row.record.skill)
        } else {
            SkillAdmission::Off
        };
        drop(state);
        self.set_admission(name, admission, expected)
    }

    /// Persist catalog ordering against an exact catalog/Settings snapshot.
    ///
    /// # Errors
    /// Stale or failed writes leave both ordering and admission unchanged.
    pub fn set_sort(
        &self,
        sort: SkillSort,
        expected: &SkillCatalogSnapshot,
    ) -> Result<SkillCatalogSnapshot, SkillPreferenceError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| SkillPreferenceError::Unavailable)?;
        if state.revision != expected.generation {
            return Err(SkillPreferenceError::StaleGeneration);
        }
        let binding = self
            .preferences_binding
            .as_ref()
            .ok_or(SkillPreferenceError::PersistenceUnavailable)?;
        let settings_snapshot = expected
            .settings_snapshot
            .as_ref()
            .ok_or(SkillPreferenceError::PersistenceUnavailable)?;
        let desired_admission = expected.admission_overrides.clone();
        let namespace = binding.namespace.clone();
        let validation_namespace = namespace.clone();
        let next = binding
            .settings
            .replace_user_automatically(
                &namespace,
                preferences::user_value(&expected.admission_overrides, sort),
                settings_snapshot,
                move |candidate| {
                    let (candidate_admission, candidate_sort) =
                        preferences::parse_value(candidate.resolved()).map_err(|message| {
                            heycode_settings::SettingsError::InvalidResolved {
                                namespace: validation_namespace.to_string(),
                                message,
                            }
                        })?;
                    if candidate_admission != desired_admission || candidate_sort != sort {
                        return Err(heycode_settings::SettingsError::InvalidResolved {
                            namespace: validation_namespace.to_string(),
                            message: "a higher-priority setting controls skill preferences"
                                .to_owned(),
                        });
                    }
                    Ok(())
                },
            )
            .map_err(skill_settings_error)?;
        drop(state);
        self.apply_preference_snapshot(next)?;
        self.catalog_snapshot()
            .map_err(|_| SkillPreferenceError::Unavailable)
    }

    fn apply_preference_snapshot(
        &self,
        snapshot: Arc<heycode_settings::SettingsSnapshot>,
    ) -> Result<(), SkillPreferenceError> {
        let next = preferences::parse_snapshot(snapshot)?;
        *self
            .preferences
            .write()
            .map_err(|_| SkillPreferenceError::Unavailable)? = next;
        Ok(())
    }

    /// Whether this service has filesystem roots that `/reload-skills` can scan.
    #[must_use]
    pub fn is_reloadable(&self) -> bool {
        !self.roots.is_empty()
    }

    /// Ordered immutable snapshot.
    ///
    /// # Errors
    /// Poisoned registry state fails loud.
    pub fn snapshot(&self) -> Result<Vec<Skill>, SkillRegistryError> {
        self.inner
            .lock()
            .map(|state| {
                active_rows(&state.entries)
                    .into_iter()
                    .map(|row| row.record.skill.clone())
                    .collect()
            })
            .map_err(|_| SkillRegistryError::Unavailable)
    }

    /// Ordered immutable snapshot with source attribution.
    ///
    /// # Errors
    /// Poisoned registry state fails loud.
    pub fn snapshot_records(&self) -> Result<Vec<SkillRecord>, SkillRegistryError> {
        self.inner
            .lock()
            .map(|state| {
                active_rows(&state.entries)
                    .into_iter()
                    .map(|row| row.record.clone())
                    .collect()
            })
            .map_err(|_| SkillRegistryError::Unavailable)
    }

    /// Lookup one skill by canonical name or an exact compatibility alias.
    ///
    /// # Errors
    /// Poisoned registry state fails loud.
    pub fn get(&self, name: &str) -> Result<Option<Skill>, SkillRegistryError> {
        self.inner
            .lock()
            .map(|state| {
                active_row_for_identity(&state.entries, name).map(|row| row.record.skill.clone())
            })
            .map_err(|_| SkillRegistryError::Unavailable)
    }

    /// Lookup one skill and enforce persisted admission at the dispatch point.
    ///
    /// # Errors
    /// Disabled rows and poisoned state fail loud. Unknown names return `None`.
    pub fn get_enabled(&self, name: &str) -> Result<Option<Skill>, SkillRegistryError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        let preferences = self
            .preferences
            .read()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        let Some(row) = active_row_for_identity(&state.entries, name) else {
            return Ok(None);
        };
        if !row_admission(row, &preferences.admission_overrides).enabled() {
            return Err(SkillRegistryError::Disabled {
                name: row.record.skill.name.clone(),
            });
        }
        Ok(Some(row.record.skill.clone()))
    }

    /// Lookup one skill and enforce model admission at the dispatch point.
    ///
    /// `name-only` remains loadable after the model chooses its advertised
    /// name. `user-only` and `off` are refused even if a stale prompt attempts
    /// to call the tool.
    ///
    /// # Errors
    /// Refused rows and poisoned state fail loud. Unknown names return `None`.
    pub fn get_model_invocable(&self, name: &str) -> Result<Option<Skill>, SkillRegistryError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        let preferences = self
            .preferences
            .read()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        let Some(row) = active_row_for_identity(&state.entries, name) else {
            return Ok(None);
        };
        match row_admission(row, &preferences.admission_overrides) {
            SkillAdmission::On | SkillAdmission::NameOnly => {
                let mut skill = row.record.skill.clone();
                skill.disable_model_invocation = false;
                Ok(Some(skill))
            }
            SkillAdmission::UserOnly => Err(SkillRegistryError::UserOnly {
                name: row.record.skill.name.clone(),
            }),
            SkillAdmission::Off => Err(SkillRegistryError::Disabled {
                name: row.record.skill.name.clone(),
            }),
        }
    }

    /// Current enabled model-visible skills in registry/source order.
    ///
    /// This keeps prompt bytes independent from the persisted UI sort while
    /// sharing the same admission predicate used by dispatch.
    ///
    /// # Errors
    /// Poisoned registry or preference state fails loud.
    pub fn model_invocable_snapshot(&self) -> Result<Vec<Skill>, SkillRegistryError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        let preferences = self
            .preferences
            .read()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        Ok(active_rows(&state.entries)
            .into_iter()
            .filter_map(|row| {
                let admission = row_admission(row, &preferences.admission_overrides);
                if !admission.model_invocable() {
                    return None;
                }
                let mut skill = row.record.skill.clone();
                skill.disable_model_invocation = false;
                if !admission.description_visible() {
                    skill.description.clear();
                }
                Some(skill)
            })
            .collect())
    }

    /// Register one late skill and return its exact disposer.
    ///
    /// # Errors
    /// Invalid/duplicate names or poisoned state fail before publication.
    pub fn register_owned(&self, skill: Skill) -> Result<SkillRegistration, SkillRegistryError> {
        self.register_owned_from_source(skill, SkillSource::contribution(), Vec::new(), false)
    }

    /// Register a late skill with exact former invocation ids.
    ///
    /// Compatibility aliases remain accepted by lookup and also migrate
    /// persisted disabled choices, but only the canonical name is advertised.
    /// Every identity must be unique across live canonical names and aliases.
    ///
    /// # Errors
    /// Invalid or contested identities fail before publication.
    pub fn register_owned_with_aliases(
        &self,
        skill: Skill,
        aliases: Vec<String>,
    ) -> Result<SkillRegistration, SkillRegistryError> {
        self.register_owned_from_source(skill, SkillSource::contribution(), aliases, false)
    }

    /// Register a frozen imported skill with its actual authority scope.
    /// Project attribution keeps dynamic-workspace admission from treating the
    /// skill as a user-wide contribution. This does not grant filesystem access.
    ///
    /// # Errors
    /// Only user/project scopes and bounded control-free provenance labels are accepted.
    pub fn register_imported_owned(
        &self,
        skill: Skill,
        scope: SkillSourceScope,
        generation_label: &str,
    ) -> Result<SkillRegistration, SkillRegistryError> {
        if !matches!(scope, SkillSourceScope::User | SkillSourceScope::Project)
            || generation_label.is_empty()
            || generation_label.len() > 128
            || generation_label.chars().any(char::is_control)
        {
            return Err(SkillRegistryError::InvalidName);
        }
        let name = skill.name.clone();
        self.register_owned_from_source(
            skill,
            SkillSource::discovered(scope, generation_label.to_owned(), name),
            Vec::new(),
            true,
        )
    }

    fn register_owned_from_source(
        &self,
        skill: Skill,
        source: SkillSource,
        mut aliases: Vec<String>,
        allow_scoped_shadow: bool,
    ) -> Result<SkillRegistration, SkillRegistryError> {
        validate_skill_name(&skill.name)?;
        for alias in &aliases {
            validate_skill_name(alias)?;
        }
        aliases.sort();
        aliases.dedup();
        aliases.retain(|alias| alias != &skill.name);
        let mut state = self
            .inner
            .lock()
            .map_err(|_| SkillRegistryError::Unavailable)?;
        let contested = std::iter::once(skill.name.as_str())
            .chain(aliases.iter().map(String::as_str))
            .find(|identity| {
                state.entries.iter().any(|row| {
                    if !row_owns_identity(row, identity) {
                        return false;
                    }
                    let canonical_shadow = allow_scoped_shadow
                        && *identity == skill.name
                        && row.record.skill.name == skill.name
                        && can_share_canonical(row, source.scope, false);
                    !canonical_shadow
                })
            });
        if let Some(contested) = contested {
            return Err(SkillRegistryError::Duplicate {
                name: contested.to_owned(),
            });
        }
        let name = skill.name.clone();
        let token = Arc::new(());
        state.entries.push(SkillEntry {
            record: SkillRecord { skill, source },
            preference_aliases: aliases,
            token: token.clone(),
            discovered: false,
        });
        state.revision = state.revision.saturating_add(1);
        Ok(SkillRegistration {
            inner: Arc::downgrade(&self.inner),
            name,
            token,
            active: true,
        })
    }

    /// Register one late skill as a context effect.
    ///
    /// # Errors
    /// Invalid/duplicate names or poisoned state fail before publication.
    pub fn register_effect(
        &self,
        context: &heycode_core::Context,
        skill: Skill,
    ) -> Result<(), SkillRegistryError> {
        let registration = self.register_owned(skill)?;
        context.effect(move || drop(registration));
        Ok(())
    }
}

impl Drop for SkillRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut state) = inner.lock() else {
            return;
        };
        let before = state.entries.len();
        state.entries.retain(|row| {
            row.record.skill.name != self.name || !Arc::ptr_eq(&row.token, &self.token)
        });
        if state.entries.len() != before {
            state.revision = state.revision.saturating_add(1);
        }
    }
}

impl Default for SkillSet {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SkillState {
                entries: Vec::new(),
                skipped: Vec::new(),
                revision: 1,
                reload_diagnostics: Vec::new(),
            })),
            roots: Arc::new(Vec::new()),
            reload_guard: Arc::new(reload::PinnedSkillReloadGuard),
            preferences: preferences::detached_preferences(),
            preferences_binding: None,
        }
    }
}

fn row_owns_identity(row: &SkillEntry, name: &str) -> bool {
    row.record.skill.name == name || row.preference_aliases.iter().any(|alias| alias == name)
}

fn active_rows(entries: &[SkillEntry]) -> Vec<&SkillEntry> {
    entries
        .iter()
        .filter(|candidate| {
            entries
                .iter()
                .filter(|row| row.record.skill.name == candidate.record.skill.name)
                .max_by_key(|row| shadow_rank(row))
                .is_some_and(|winner| Arc::ptr_eq(&winner.token, &candidate.token))
        })
        .collect()
}

fn active_row_for_identity<'a>(entries: &'a [SkillEntry], name: &str) -> Option<&'a SkillEntry> {
    if let Some(row) = entries
        .iter()
        .find(|row| row.preference_aliases.iter().any(|alias| alias == name))
    {
        return active_rows(entries)
            .into_iter()
            .find(|active| Arc::ptr_eq(&active.token, &row.token));
    }
    entries
        .iter()
        .filter(|row| row.record.skill.name == name)
        .max_by_key(|row| shadow_rank(row))
}

fn shadow_rank(row: &SkillEntry) -> (u8, u8) {
    let scope = match row.record.source.scope {
        SkillSourceScope::Project => 3,
        SkillSourceScope::User => 2,
        SkillSourceScope::Configured => 1,
        SkillSourceScope::Contribution => 0,
    };
    (scope, u8::from(row.discovered))
}

fn can_share_canonical(existing: &SkillEntry, scope: SkillSourceScope, discovered: bool) -> bool {
    let existing_scope = existing.record.source.scope;
    if !matches!(
        existing_scope,
        SkillSourceScope::Project | SkillSourceScope::User
    ) || !matches!(scope, SkillSourceScope::Project | SkillSourceScope::User)
    {
        return false;
    }
    existing_scope != scope || existing.discovered != discovered
}

fn default_admission(skill: &Skill) -> SkillAdmission {
    if skill.disable_model_invocation {
        SkillAdmission::UserOnly
    } else {
        SkillAdmission::On
    }
}

fn row_admission(row: &SkillEntry, overrides: &preferences::AdmissionOverrides) -> SkillAdmission {
    let identities = std::iter::once(row.record.skill.name.as_str())
        .chain(row.preference_aliases.iter().map(String::as_str));
    let mut admission = default_admission(&row.record.skill);
    for identity in identities {
        if overrides.disabled.contains(identity) {
            return SkillAdmission::Off;
        }
        if overrides.user_only.contains(identity) {
            admission = SkillAdmission::UserOnly;
        } else if overrides.name_only.contains(identity)
            && !row.record.skill.disable_model_invocation
            && admission != SkillAdmission::UserOnly
        {
            admission = SkillAdmission::NameOnly;
        }
    }
    admission
}

fn source_sort_key(record: &SkillRecord) -> (u8, &str, &str) {
    let rank = match record.source.scope {
        SkillSourceScope::Project => 0,
        SkillSourceScope::User => 1,
        SkillSourceScope::Configured => 2,
        SkillSourceScope::Contribution => 3,
    };
    (rank, &record.source.root, &record.source.directory)
}

fn skill_settings_error(error: heycode_settings::SettingsError) -> SkillPreferenceError {
    SkillPreferenceError::Settings {
        message: error.to_string(),
    }
}

fn validate_skill_name(name: &str) -> Result<(), SkillRegistryError> {
    if name.is_empty()
        || name.len() > 128
        || name.trim() != name
        || name.chars().any(char::is_control)
        || name.chars().any(char::is_whitespace)
    {
        return Err(SkillRegistryError::InvalidName);
    }
    Ok(())
}

/// Default discovery roots: project first, then the harness home.
///
/// # Errors
/// Either authority must be absolute and have a stable existing directory
/// ancestor; unsafe filesystem state fails without creating either root.
pub fn default_roots(project: &Path, home: &Path) -> Result<Vec<SkillRoot>, io::Error> {
    let (project, project_suffix) = open_authority(project)?;
    let (home, home_suffix) = open_authority(home)?;
    let project = Arc::new(project);
    let home = Arc::new(home);
    Ok(vec![
        SkillRoot::from_authority(
            project.clone(),
            project_suffix.join(".heycode/skills"),
            SkillSourceScope::Project,
        )?,
        SkillRoot::from_authority(
            project,
            project_suffix.join(".agents/skills"),
            SkillSourceScope::Project,
        )?,
        SkillRoot::from_authority(home, home_suffix.join("skills"), SkillSourceScope::User)?,
    ])
}

/// Bind only the user's configured `$HEYCODE_HOME/skills` authority.
///
/// # Errors
/// The home authority must satisfy the same stable absolute/no-follow rules as
/// [`SkillRoot::bind`].
pub fn user_root(home: &Path) -> Result<SkillRoot, io::Error> {
    let (home, home_suffix) = open_authority(home)?;
    SkillRoot::from_authority(
        Arc::new(home),
        home_suffix.join("skills"),
        SkillSourceScope::User,
    )
}
