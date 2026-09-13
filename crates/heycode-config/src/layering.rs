//! Multi-file loading: the home file is the base, a trusted project file
//! layers over it key by key, an explicit `--config` replaces both.
//!
//! Unknown keys are warnings, never startup failures — a typo in one line of
//! a project file should not lock the user out of the product — and a type
//! error names the file, the dotted key, and the line so it can be fixed
//! without a search.

use std::path::{Path, PathBuf};

use crate::{
    CONFIG_SCHEMA_VERSION, Config, ConfigError, ConfigMigrationDisposition, ConfigMigrationPlan,
    ConfigSource, ConfigVersionState, LoadedConfig, MigrationApplyOutcome, migration,
};

/// The candidate files one startup may read, already discovered.
///
/// Discovery (cwd, `$HEYCODE_HOME`, workspace trust) belongs to the caller so
/// loading itself is a pure function of paths and testable without touching
/// process globals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigPaths {
    /// `--config <path>`: the whole authority when present.
    pub explicit: Option<PathBuf>,
    /// `./heycode.toml`, only when the workspace is trusted for settings.
    pub project: Option<PathBuf>,
    /// `$HEYCODE_HOME/config.toml`.
    pub home: Option<PathBuf>,
}

/// A non-fatal finding from loading configuration, shown once at startup.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigWarning {
    /// A key no section of the schema knows; it was ignored.
    #[error("{}: unknown key `{key}` ignored (check spelling against `heycode config show`)", path.display())]
    UnknownKey {
        /// File that carries the key.
        path: PathBuf,
        /// Dotted key path, e.g. `llm.modle`.
        key: String,
    },
}

/// One parsed file plus the keys it carried that the schema does not know.
struct ParsedFile {
    path: PathBuf,
    table: toml::Table,
    unknown: Vec<String>,
}

/// One file that contributed to the effective configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLayer {
    /// File path as discovered.
    pub path: PathBuf,
    /// Its parsed contents (secrets included — never render this directly).
    pub table: toml::Table,
}

impl ParsedFile {
    fn layer(&self) -> ConfigLayer {
        ConfigLayer {
            path: self.path.clone(),
            table: self.table.clone(),
        }
    }
}

impl Config {
    /// Load configuration from already-discovered paths.
    ///
    /// `explicit` alone wins when present. Otherwise `home` is the base and
    /// `project` is layered over it key by key (tables merge, everything else
    /// replaces). Only a home file is migrated automatically; an explicit file
    /// reports a pending migration; a project overlay is never a migration
    /// subject because it is a partial layer, not a document of its own.
    ///
    /// # Errors
    /// I/O, malformed TOML, a type error (named by file, key and line), a
    /// newer schema, or a migration backup conflict.
    pub fn load_paths(
        paths: ConfigPaths,
        current_builtin_profile: &[&str],
    ) -> Result<LoadedConfig, ConfigError> {
        let mut warnings = Vec::new();
        if let Some(path) = paths.explicit {
            let migration = plan_notice(&path, current_builtin_profile, false)?;
            let parsed = parse_file(&path)?;
            warnings.extend(parsed.warnings());
            let mut config = deserialize(parsed.table.clone(), &[&parsed])?;
            config.layers = vec![parsed.layer()];
            return Ok(LoadedConfig {
                config,
                source: ConfigSource::Explicit(path),
                migration,
                warnings,
            });
        }
        let home = paths
            .home
            .filter(|path| path.is_file())
            .map(|path| {
                let migration = plan_notice(&path, current_builtin_profile, true)?;
                let parsed = parse_file(&path)?;
                Ok::<_, ConfigError>((parsed, migration))
            })
            .transpose()?;
        let project = paths
            .project
            .filter(|path| path.is_file())
            .map(|path| parse_file(&path))
            .transpose()?;
        match (home, project) {
            (None, None) => Ok(LoadedConfig {
                config: Self::defaults(),
                source: ConfigSource::BuiltIn,
                migration: None,
                warnings,
            }),
            (Some((home, migration)), None) => {
                warnings.extend(home.warnings());
                let mut config = deserialize(home.table.clone(), &[&home])?;
                config.layers = vec![home.layer()];
                Ok(LoadedConfig {
                    config,
                    source: ConfigSource::Home(home.path),
                    migration,
                    warnings,
                })
            }
            (None, Some(project)) => {
                warnings.extend(project.warnings());
                let mut config = deserialize(project.table.clone(), &[&project])?;
                config.layers = vec![project.layer()];
                Ok(LoadedConfig {
                    config,
                    source: ConfigSource::Project(project.path),
                    migration: None,
                    warnings,
                })
            }
            (Some((home, migration)), Some(project)) => {
                warnings.extend(home.warnings());
                warnings.extend(project.warnings());
                let mut merged = home.table.clone();
                merge_tables(&mut merged, project.table.clone());
                let mut config = deserialize(merged, &[&home, &project])?;
                config.layers = vec![home.layer(), project.layer()];
                Ok(LoadedConfig {
                    config,
                    source: ConfigSource::Layered {
                        home: home.path,
                        project: project.path,
                    },
                    migration,
                    warnings,
                })
            }
        }
    }
}

/// Where one effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValueSource {
    /// Compiled default; no file or flag set it.
    Default,
    /// The highest-layer file that carries the key.
    File(PathBuf),
    /// A command-line flag or `--set` for this process.
    Flag,
    /// An admitted settings field applied after legacy configuration layers.
    Settings(String),
}

impl std::fmt::Display for ConfigValueSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => formatter.write_str("default"),
            Self::File(path) => write!(formatter, "{}", path.display()),
            Self::Flag => formatter.write_str("command line"),
            Self::Settings(source) => formatter.write_str(source),
        }
    }
}

/// One effective leaf value with its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRow {
    /// Dotted key, e.g. `llm.model`.
    pub key: String,
    /// TOML-rendered value; secret-shaped positions are replaced by `•••`.
    pub value: String,
    /// Which layer supplied it.
    pub source: ConfigValueSource,
}

/// Every effective value with the layer it came from — what `/config` and
/// `heycode config show` print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigReport {
    layers: Vec<PathBuf>,
    rows: Vec<ConfigRow>,
}

/// Placeholder for values that may be credentials (MCP server environments).
const REDACTED: &str = "•••";

impl ConfigReport {
    /// Build the report for `effective` (after flags) from the files that
    /// contributed to it, lowest layer first.
    #[must_use]
    pub fn new(effective: &Config, layers: &[ConfigLayer]) -> Self {
        let table = toml::Value::try_from(effective)
            .ok()
            .and_then(|value| value.try_into::<toml::Table>().ok())
            .unwrap_or_default();
        let mut rows = Vec::new();
        flatten(&table, &mut Vec::new(), &mut rows);
        let rows = rows
            .into_iter()
            .map(|(key, value)| {
                let source = if effective.is_patched(&key) {
                    ConfigValueSource::Flag
                } else if let Some(source) = effective.effective_sources.get(&key) {
                    source.clone()
                } else {
                    layers
                        .iter()
                        .rev()
                        .find(|layer| table_has_key(&layer.table, &key))
                        .map_or(ConfigValueSource::Default, |layer| {
                            ConfigValueSource::File(layer.path.clone())
                        })
                };
                let value = if is_secret_position(&key) {
                    REDACTED.to_owned()
                } else {
                    render_value(&value)
                };
                ConfigRow { key, value, source }
            })
            .collect();
        Self {
            layers: layers.iter().map(|layer| layer.path.clone()).collect(),
            rows,
        }
    }

    /// Effective rows in key order.
    #[must_use]
    pub fn rows(&self) -> &[ConfigRow] {
        &self.rows
    }

    /// Human-readable listing: a header, the layer chain, then one line per
    /// value with its source in parentheses.
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines = vec!["config".to_owned()];
        let mut chain: Vec<String> = vec!["defaults".to_owned()];
        chain.extend(self.layers.iter().map(|path| path.display().to_string()));
        if self
            .rows
            .iter()
            .any(|row| row.source == ConfigValueSource::Flag)
        {
            chain.push("command line".to_owned());
        }
        lines.push(format!("layers: {}", chain.join(" → ")));
        lines.extend(
            self.rows
                .iter()
                .map(|row| format!("{} = {}  ({})", row.key, row.value, row.source)),
        );
        lines.join("\n")
    }
}

/// MCP server environments are the one place a config file may legitimately
/// carry a token; everything else here is routing and limits.
fn is_secret_position(key: &str) -> bool {
    let mut parts = key.split('.');
    matches!(
        (parts.next(), parts.next(), parts.nth(1), parts.next()),
        (Some("mcp"), Some("servers"), Some("env"), Some(_))
    )
}

/// TOML rendering, except that floats are shown at `f32` precision because
/// every float setting is an `f32` and `0.800000011920929` is not what the
/// user wrote.
fn render_value(value: &toml::Value) -> String {
    match value {
        #[allow(clippy::cast_possible_truncation)]
        toml::Value::Float(float) => (*float as f32).to_string(),
        other => other.to_string(),
    }
}

fn flatten(table: &toml::Table, path: &mut Vec<String>, out: &mut Vec<(String, toml::Value)>) {
    for (key, value) in table {
        path.push(key.clone());
        match value {
            toml::Value::Table(inner) => flatten(inner, path, out),
            other => out.push((path.join("."), other.clone())),
        }
        path.pop();
    }
}

impl ParsedFile {
    fn warnings(&self) -> impl Iterator<Item = ConfigWarning> + '_ {
        self.unknown.iter().map(|key| ConfigWarning::UnknownKey {
            path: self.path.clone(),
            key: key.clone(),
        })
    }
}

fn plan_notice(
    path: &Path,
    current_builtin_profile: &[&str],
    apply: bool,
) -> Result<Option<crate::ConfigMigrationNotice>, ConfigError> {
    let Some(plan) = ConfigMigrationPlan::read(path, current_builtin_profile)? else {
        return Ok(None);
    };
    let disposition = if apply {
        match plan.apply()? {
            MigrationApplyOutcome::AlreadyApplied { .. } => return Ok(None),
            MigrationApplyOutcome::Applied { backup_path } => {
                ConfigMigrationDisposition::Applied { backup_path }
            }
        }
    } else {
        ConfigMigrationDisposition::Pending
    };
    Ok(Some(plan.notice(disposition)))
}

/// Read and parse one file into a TOML table, refusing a newer schema and
/// collecting the dotted keys the [`Config`] schema does not know.
fn parse_file(path: &Path) -> Result<ParsedFile, ConfigError> {
    let label = path.display().to_string();
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: label.clone(),
        source,
    })?;
    if let ConfigVersionState::Newer(found) = migration::classify_document(&raw, &label)? {
        return Err(ConfigError::NewerSchema {
            path: label,
            found,
            supported: CONFIG_SCHEMA_VERSION,
        });
    }
    let mut table: toml::Table =
        raw.parse()
            .map_err(|error: toml::de::Error| ConfigError::Parse {
                path: label.clone(),
                message: error.message().to_owned(),
            })?;
    migration::normalize_legacy_approval(&mut table);
    let mut unknown = Vec::new();
    // A dry deserialization whose only purpose is to learn which keys were
    // ignored; the merged document is deserialized for real afterwards. Type
    // errors are reported there with their location, so they are ignored here.
    let _ = serde_ignored::deserialize::<_, _, Config>(table.clone(), |ignored| {
        unknown.push(ignored.to_string());
    });
    unknown.sort();
    Ok(ParsedFile {
        path: path.to_path_buf(),
        table,
        unknown,
    })
}

/// Deserialize the final table, naming file, dotted key and line on a type
/// error so the user does not have to hunt for it.
///
/// `sources` are the contributing files, lowest layer first; the error is
/// attributed to the highest layer that actually carries the offending key.
fn deserialize(table: toml::Table, sources: &[&ParsedFile]) -> Result<Config, ConfigError> {
    let fallback = sources
        .last()
        .map(|file| file.path.display().to_string())
        .unwrap_or_default();
    let rendered = toml::to_string(&table).map_err(|error| ConfigError::Parse {
        path: fallback.clone(),
        message: error.to_string(),
    })?;
    toml::from_str(&rendered).map_err(|error| {
        let key = error
            .span()
            .and_then(|span| key_path_at(&rendered, span.start));
        let owner = key.as_deref().and_then(|key| {
            sources
                .iter()
                .rev()
                .find(|file| table_has_key(&file.table, key))
        });
        let line = match (owner, key.as_deref()) {
            (Some(file), Some(key)) => line_of_key(&file.path, key)
                .map(|line| format!(" (line {line})"))
                .unwrap_or_default(),
            _ => String::new(),
        };
        let key = key.map(|key| format!(" at `{key}`")).unwrap_or_default();
        ConfigError::Parse {
            path: owner.map_or(fallback.clone(), |file| file.path.display().to_string()),
            message: format!("{}{key}{line}", error.message()),
        }
    })
}

fn table_has_key(table: &toml::Table, dotted: &str) -> bool {
    let mut current = table;
    let mut parts = dotted.split('.').peekable();
    while let Some(part) = parts.next() {
        match current.get(part) {
            Some(toml::Value::Table(inner)) if parts.peek().is_some() => current = inner,
            Some(_) => return parts.peek().is_none(),
            None => return false,
        }
    }
    false
}

/// 1-based line of the value for `dotted` in the file at `path`, found by
/// re-parsing it with spans; the merged rendering's line numbers mean nothing
/// to the user.
fn line_of_key(path: &Path, dotted: &str) -> Option<usize> {
    let raw = std::fs::read_to_string(path).ok()?;
    let document: toml_edit::ImDocument<&str> = toml_edit::ImDocument::parse(raw.as_str()).ok()?;
    let mut item: &toml_edit::Item = document.as_item();
    for part in dotted.split('.') {
        item = item.get(part)?;
    }
    let offset = item.as_value()?.span()?.start;
    Some(raw[..offset].matches('\n').count() + 1)
}

/// Dotted key whose value span contains `offset` in `raw`.
fn key_path_at(raw: &str, offset: usize) -> Option<String> {
    let document: toml_edit::ImDocument<&str> = toml_edit::ImDocument::parse(raw).ok()?;
    let mut path = Vec::new();
    find_in_table(document.as_table(), offset, &mut path).then(|| path.join("."))
}

fn find_in_table(table: &toml_edit::Table, offset: usize, path: &mut Vec<String>) -> bool {
    for (key, item) in table.iter() {
        path.push(key.to_owned());
        if let Some(inner) = item.as_table() {
            if find_in_table(inner, offset, path) {
                return true;
            }
        } else if let Some(value) = item.as_value() {
            if value.span().is_some_and(|span| span.contains(&offset)) {
                return true;
            }
        } else if let Some(array) = item.as_array_of_tables() {
            for inner in array.iter() {
                if find_in_table(inner, offset, path) {
                    return true;
                }
            }
        }
        path.pop();
    }
    false
}

/// Layer `over` onto `under`: tables merge recursively, everything else
/// (including arrays) is replaced whole.
fn merge_tables(under: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (under.get_mut(&key), value) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(incoming)) => {
                merge_tables(existing, incoming);
            }
            (_, value) => {
                under.insert(key, value);
            }
        }
    }
}
