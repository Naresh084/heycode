//! S14 settings UI: a renderable form derived from a namespace's schema, or a
//! plugin's own panel when the derived form is not good enough.
//!
//! The whole point is that a plugin gets a settings surface for free by
//! declaring a schema, and can opt out into a custom panel without the host
//! learning anything about that plugin. Both are UI-neutral: this crate decides
//! *what* to show, `heycode-tui` decides how.
//!
//! One rule shapes the model more than any other: **a secret's value never
//! enters a form.** Not "is redacted before rendering" — never enters. A
//! [`SettingsField::Secret`] has nowhere to put a value, so no renderer, log or
//! snapshot can leak one, and no future edit to a render path can reintroduce
//! the bug.

use heycode_settings::{SettingsNamespace, SettingsSnapshot};
use serde_json::Value;

use crate::{UiContributionId, UiRegistryError};

/// Where a field's effective value came from, so the UI can say why a value is
/// what it is — and why it may not be editable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldOrigin {
    /// The schema's default; nothing has overridden it.
    Default,
    /// A user-level value.
    User,
    /// A project-level value.
    Project,
    /// An administrator-managed value. Locked: editing here cannot win.
    Managed,
}

impl FieldOrigin {
    /// Whether a UI should offer to edit this field.
    ///
    /// A managed value is shown read-only rather than editable-then-rejected:
    /// discovering a lock by being denied is a poor experience, and an edit
    /// silently overridden at the next resolve is worse.
    #[must_use]
    pub const fn editable(self) -> bool {
        !matches!(self, Self::Managed)
    }
}

/// One renderable settings control.
///
/// `Secret` deliberately carries no value field. That is the entire redaction
/// strategy: there is nowhere for a secret to be, so it cannot be rendered by
/// accident.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SettingsField {
    /// A boolean.
    Toggle {
        /// Dotted path within the namespace.
        path: String,
        /// Effective value.
        value: bool,
        /// Where it came from.
        origin: FieldOrigin,
    },
    /// Free text.
    Text {
        /// Dotted path within the namespace.
        path: String,
        /// Effective value.
        value: String,
        /// Where it came from.
        origin: FieldOrigin,
    },
    /// A number, kept as text so no precision is lost on the way to a control.
    Number {
        /// Dotted path within the namespace.
        path: String,
        /// Effective value, rendered exactly as the document holds it.
        value: String,
        /// Where it came from.
        origin: FieldOrigin,
    },
    /// A closed set of allowed values.
    Choice {
        /// Dotted path within the namespace.
        path: String,
        /// Allowed values in schema order.
        options: Vec<String>,
        /// Effective value, when it is one of the options.
        selected: Option<String>,
        /// Where it came from.
        origin: FieldOrigin,
    },
    /// Credential material. Carries whether something is configured, never what.
    Secret {
        /// Dotted path within the namespace.
        path: String,
        /// Whether a value exists, which is all a UI may know.
        configured: bool,
        /// Where it came from.
        origin: FieldOrigin,
    },
    /// A schema construct this build cannot render.
    ///
    /// Reported rather than dropped: a field the user cannot see is a field
    /// they cannot fix, and silently omitting it makes the form lie about what
    /// the namespace contains.
    Unrenderable {
        /// Dotted path within the namespace.
        path: String,
        /// Why, as a closed reason.
        reason: UnrenderableReason,
    },
}

impl SettingsField {
    /// The field's dotted path.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Toggle { path, .. }
            | Self::Text { path, .. }
            | Self::Number { path, .. }
            | Self::Choice { path, .. }
            | Self::Secret { path, .. }
            | Self::Unrenderable { path, .. } => path,
        }
    }

    /// Whether a UI should offer to edit this field.
    #[must_use]
    pub const fn editable(&self) -> bool {
        match self {
            Self::Toggle { origin, .. }
            | Self::Text { origin, .. }
            | Self::Number { origin, .. }
            | Self::Choice { origin, .. }
            | Self::Secret { origin, .. } => origin.editable(),
            Self::Unrenderable { .. } => false,
        }
    }
}

/// Why a schema position could not become a control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnrenderableReason {
    /// The schema declares no type this build knows how to render.
    UnsupportedType,
    /// The schema nests deeper than the renderer walks.
    TooDeep,
    /// An open-ended map or array, which needs a custom panel.
    OpenEnded,
}

impl std::fmt::Display for UnrenderableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedType => "this build cannot render the declared type",
            Self::TooDeep => "the schema nests deeper than the renderer walks",
            Self::OpenEnded => "an open-ended map or array needs a custom panel",
        })
    }
}

/// A namespace's settings surface, derived or delegated.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SettingsSurface {
    /// Controls derived from the namespace schema.
    Derived(SettingsForm),
    /// A plugin's own panel, registered in the UI registry under this id.
    Custom(UiContributionId),
}

/// Controls for one namespace, in schema order.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsForm {
    namespace: String,
    fields: Vec<SettingsField>,
}

impl SettingsForm {
    /// The namespace this form renders.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Controls in schema order.
    #[must_use]
    pub fn fields(&self) -> &[SettingsField] {
        &self.fields
    }

    /// Fields this build could not render, if any.
    ///
    /// A UI shows these as a visible gap so a user knows to edit the file
    /// directly rather than believing the form is complete.
    #[must_use]
    pub fn unrenderable(&self) -> Vec<&SettingsField> {
        self.fields
            .iter()
            .filter(|field| matches!(field, SettingsField::Unrenderable { .. }))
            .collect()
    }
}

/// Maximum schema nesting the derived renderer walks.
const MAX_DEPTH: usize = 4;

/// Derive a form from one namespace snapshot.
///
/// # Errors
/// Never fails on schema content: an unrenderable construct becomes an
/// [`SettingsField::Unrenderable`] row rather than an error, because a
/// namespace with one odd field must still show the rest.
pub fn derive_form(snapshot: &SettingsSnapshot) -> SettingsForm {
    let mut fields = Vec::new();
    walk(
        snapshot,
        snapshot.schema(),
        snapshot.resolved(),
        &mut Vec::new(),
        0,
        &mut fields,
    );
    SettingsForm {
        namespace: snapshot.namespace().as_str().to_owned(),
        fields,
    }
}

fn walk(
    snapshot: &SettingsSnapshot,
    schema: &Value,
    resolved: &Value,
    steps: &mut Vec<String>,
    depth: usize,
    out: &mut Vec<SettingsField>,
) {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        if !steps.is_empty() {
            out.push(SettingsField::Unrenderable {
                path: steps.join("."),
                reason: UnrenderableReason::OpenEnded,
            });
        }
        return;
    };
    for (key, property) in properties {
        steps.push(key.clone());
        let path = steps.join(".");
        let value = resolved.get(key);
        let origin = origin_of(snapshot, &path);

        // The declared-secret check comes first and the name screen second: an
        // explicit declaration is authoritative, and the name screen catches a
        // field whose author forgot to declare it.
        if is_secret(snapshot, &path, key) {
            out.push(SettingsField::Secret {
                path: path.clone(),
                configured: value.is_some_and(|value| !value.is_null()),
                origin,
            });
        } else if property.get("properties").is_some() {
            if depth + 1 >= MAX_DEPTH {
                out.push(SettingsField::Unrenderable {
                    path: path.clone(),
                    reason: UnrenderableReason::TooDeep,
                });
            } else {
                walk(
                    snapshot,
                    property,
                    value.unwrap_or(&Value::Null),
                    steps,
                    depth + 1,
                    out,
                );
            }
        } else {
            out.push(control(property, &path, value, origin));
        }
        steps.pop();
    }
}

fn control(
    property: &Value,
    path: &str,
    value: Option<&Value>,
    origin: FieldOrigin,
) -> SettingsField {
    if let Some(options) = property.get("enum").and_then(Value::as_array) {
        let options: Vec<String> = options
            .iter()
            .filter_map(|option| option.as_str().map(ToOwned::to_owned))
            .collect();
        let selected = value
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .filter(|current| options.contains(current));
        return SettingsField::Choice {
            path: path.to_owned(),
            options,
            selected,
            origin,
        };
    }
    match property.get("type").and_then(Value::as_str) {
        Some("boolean") => SettingsField::Toggle {
            path: path.to_owned(),
            value: value.and_then(Value::as_bool).unwrap_or(false),
            origin,
        },
        Some("string") => SettingsField::Text {
            path: path.to_owned(),
            value: value
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_default(),
            origin,
        },
        Some("integer" | "number") => SettingsField::Number {
            path: path.to_owned(),
            // Rendered from the document's own text so a large integer or a
            // trailing zero is not silently reshaped on the way to a control.
            value: value.map_or_else(String::new, ToString::to_string),
            origin,
        },
        Some("object") => SettingsField::Unrenderable {
            path: path.to_owned(),
            reason: UnrenderableReason::OpenEnded,
        },
        _ => SettingsField::Unrenderable {
            path: path.to_owned(),
            reason: UnrenderableReason::UnsupportedType,
        },
    }
}

/// Two screens, in order of authority.
///
/// A wire-exposed namespace has already been screened by S15, and its
/// `redacted_paths` are the authoritative answer — including a field whose name
/// looks innocuous but was declared secret. For a namespace with no wire
/// projection there is no declaration to consult, so the name screen stands
/// alone. It is the same shared recognizer Q08 uses; there is exactly one list
/// of what a credential looks like.
fn is_secret(snapshot: &SettingsSnapshot, path: &str, key: &str) -> bool {
    snapshot.wire_projection().is_some_and(|projection| {
        projection
            .redacted_paths()
            .iter()
            .any(|redacted| redacted == path)
    }) || heycode_settings::names_credential_material(key)
}

fn origin_of(snapshot: &SettingsSnapshot, path: &str) -> FieldOrigin {
    // `managed_locks` is the settings layer's own list of managed leaf paths.
    // Reading the managed section ourselves would duplicate it — and a
    // duplicated check is one no test can distinguish, which is how a rule ends
    // up unpinned. One source, and it is the authoritative one.
    if snapshot.managed_locks().iter().any(|locked| locked == path) {
        return FieldOrigin::Managed;
    }
    // Below the lock, the origin a UI must report is the highest-precedence
    // layer that actually carries a value.
    for (layer, origin) in [
        (snapshot.project(), FieldOrigin::Project),
        (snapshot.user(), FieldOrigin::User),
    ] {
        if layer.is_some_and(|section| lookup(section, path).is_some()) {
            return origin;
        }
    }
    FieldOrigin::Default
}

fn lookup<'a>(section: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = section;
    for step in path.split('.') {
        current = current.get(step)?;
    }
    Some(current)
}

/// Which namespaces have a custom panel instead of a derived form.
#[derive(Debug)]
struct CustomSurface {
    contribution: UiContributionId,
    token: std::sync::Arc<()>,
}

/// Registry that selects a custom settings surface or derives one from schema.
#[derive(Debug, Clone, Default)]
pub struct SettingsUiRegistry {
    custom: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, CustomSurface>>>,
}

impl SettingsUiRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim a namespace for a custom panel.
    ///
    /// # Errors
    /// [`UiRegistryError::DuplicateContribution`] when a namespace already has
    /// a custom panel — two plugins silently competing to own one namespace's
    /// UI is a collision, not a merge.
    pub fn register_custom(
        &self,
        context: &heycode_core::Context,
        namespace: &SettingsNamespace,
        contribution: UiContributionId,
    ) -> Result<(), UiRegistryError> {
        let mut custom = self
            .custom
            .lock()
            .map_err(|_| UiRegistryError::RegistryUnavailable)?;
        if custom.contains_key(namespace.as_str()) {
            return Err(UiRegistryError::Duplicate {
                identity: format!("settings:{}", namespace.as_str()),
            });
        }
        let namespace = namespace.as_str().to_owned();
        let token = std::sync::Arc::new(());
        custom.insert(
            namespace.clone(),
            CustomSurface {
                contribution,
                token: token.clone(),
            },
        );
        drop(custom);
        let registry = std::sync::Arc::downgrade(&self.custom);
        context.effect(move || {
            let Some(registry) = registry.upgrade() else {
                return;
            };
            let Ok(mut custom) = registry.lock() else {
                return;
            };
            if custom
                .get(&namespace)
                .is_some_and(|current| std::sync::Arc::ptr_eq(&current.token, &token))
            {
                custom.remove(&namespace);
            }
        });
        Ok(())
    }

    /// The surface for one namespace: its custom panel, or a derived form.
    ///
    /// # Errors
    /// A poisoned registry fails loud.
    pub fn surface(&self, snapshot: &SettingsSnapshot) -> Result<SettingsSurface, UiRegistryError> {
        let custom = self
            .custom
            .lock()
            .map_err(|_| UiRegistryError::RegistryUnavailable)?;
        Ok(custom.get(snapshot.namespace().as_str()).map_or_else(
            || SettingsSurface::Derived(derive_form(snapshot)),
            |surface| SettingsSurface::Custom(surface.contribution.clone()),
        ))
    }
}
