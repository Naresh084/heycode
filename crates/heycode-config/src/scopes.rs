//! Deterministic plugin enable/disable resolution across activation scopes.

use std::collections::BTreeSet;

use heycode_core::PluginScope;

use crate::ConfigError;

/// One enable/disable row in a scoped overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginDirective {
    /// Ensure the plugin is present; an existing row keeps its position and
    /// adopts this higher winning scope.
    Enable(String),
    /// Remove the effective plugin row when present.
    Disable(String),
}

impl PluginDirective {
    /// Enable one plugin id.
    #[must_use]
    pub fn enable(id: impl Into<String>) -> Self {
        Self::Enable(id.into())
    }

    /// Disable one plugin id.
    #[must_use]
    pub fn disable(id: impl Into<String>) -> Self {
        Self::Disable(id.into())
    }

    fn id(&self) -> &str {
        match self {
            Self::Enable(id) | Self::Disable(id) => id,
        }
    }
}

/// One complete overlay at a unique non-built-in scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginScopeLayer {
    /// Layer precedence identity.
    pub scope: PluginScope,
    /// Ordered directives within the layer.
    pub directives: Vec<PluginDirective>,
}

impl PluginScopeLayer {
    /// Construct a layer; resolution validates scope/id uniqueness.
    #[must_use]
    pub fn new(scope: PluginScope, directives: Vec<PluginDirective>) -> Self {
        Self { scope, directives }
    }
}

/// One enabled plugin after all scope overlays resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePluginSelection {
    /// Stable plugin/factory id.
    pub id: String,
    /// Highest scope whose enable currently wins.
    pub scope: PluginScope,
}

/// Resolve built-in base order plus unique overlays. Caller layer order is
/// irrelevant; scope precedence is fixed by [`PluginScope::precedence`].
/// Existing enabled rows retain their position, newly enabled rows append in
/// directive order, and re-enabling a removed row appends at that point.
///
/// # Errors
/// Invalid/duplicate plugin ids, duplicate scopes, or a built-in overlay.
pub fn resolve_scoped_plugins(
    built_in: &[&str],
    layers: &[PluginScopeLayer],
) -> Result<Vec<EffectivePluginSelection>, ConfigError> {
    let mut seen_base = BTreeSet::new();
    let mut effective = Vec::with_capacity(built_in.len());
    for id in built_in {
        validate_plugin_id(id)?;
        if !seen_base.insert(*id) {
            return scope_error(format!("duplicate built-in plugin `{id}`"));
        }
        effective.push(EffectivePluginSelection {
            id: (*id).to_owned(),
            scope: PluginScope::BuiltIn,
        });
    }

    let mut seen_scopes = BTreeSet::new();
    let mut ordered: Vec<&PluginScopeLayer> = layers.iter().collect();
    for layer in &ordered {
        if layer.scope == PluginScope::BuiltIn {
            return scope_error("built_in is the base, not an overlay scope");
        }
        if !seen_scopes.insert(layer.scope) {
            return scope_error(format!(
                "plugin scope `{}` appears more than once",
                layer.scope.as_str()
            ));
        }
    }
    ordered.sort_by_key(|layer| layer.scope.precedence());

    for layer in ordered {
        let mut seen_ids = BTreeSet::new();
        for directive in &layer.directives {
            let id = directive.id();
            validate_plugin_id(id)?;
            if !seen_ids.insert(id) {
                return scope_error(format!(
                    "plugin `{id}` appears more than once in scope `{}`",
                    layer.scope.as_str()
                ));
            }
            match directive {
                PluginDirective::Enable(id) => {
                    if let Some(row) = effective.iter_mut().find(|row| row.id == *id) {
                        row.scope = layer.scope;
                    } else {
                        effective.push(EffectivePluginSelection {
                            id: id.clone(),
                            scope: layer.scope,
                        });
                    }
                }
                PluginDirective::Disable(id) => {
                    effective.retain(|row| row.id != *id);
                }
            }
        }
    }
    Ok(effective)
}

pub(crate) fn validate_plugin_id(id: &str) -> Result<(), ConfigError> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !id.ends_with('-')
        && !id.contains("--");
    if valid {
        Ok(())
    } else {
        scope_error(format!(
            "invalid plugin id `{id}`; expected lowercase kebab-case"
        ))
    }
}

fn scope_error<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Parse {
        path: "<plugin-scopes>".to_owned(),
        message: message.into(),
    })
}
