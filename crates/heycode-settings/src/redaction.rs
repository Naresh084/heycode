//! Secret field roles, wire-exposure verification and redaction.
//!
//! Exposure is proved, not asserted. A namespace that attests wire exposure
//! must classify every path it would project: an owner role discharges a
//! path explicitly, and every undeclared path must survive two screens that
//! can only ever refuse. Anything left unclassified is unprovable and the
//! whole namespace fails exposure.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::SettingsError;

/// Text substituted for any redacted settings value or map key.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Recursion bound for schema-metadata inspection.
const MAX_SCHEMA_DEPTH: usize = 32;

/// Why a namespace could not be proved safe to project.
///
/// The reason is a closed set precisely so a fault can never carry the
/// offending value into an error, a log or a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireExposureFault {
    /// The key names credential material and no owner role covers the path.
    SecretShapedKey,
    /// A key or value matches a recognized credential format.
    CredentialMaterial,
    /// One path is declared both secret and public.
    ContradictoryRole,
}

impl std::fmt::Display for WireExposureFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SecretShapedKey => {
                "the key names credential material and no owner role covers it"
            }
            Self::CredentialMaterial => "a key or value matches a recognized credential format",
            Self::ContradictoryRole => "the path is declared both secret and public",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum PathSegment {
    Key(String),
    Wildcard,
}

/// Dot-separated path to one field inside a settings namespace.
///
/// `*` matches exactly one map key or array index. A named segment matches
/// only a map key, so a declared literal never silently covers a generic
/// schema position.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SettingsFieldPath(Vec<PathSegment>);

impl SettingsFieldPath {
    /// Parse a dot-separated field path.
    ///
    /// # Errors
    /// [`SettingsError::InvalidFieldPath`] when the path is empty, has an
    /// empty segment, or mixes `*` with other characters in one segment.
    pub fn new(value: impl Into<String>) -> Result<Self, SettingsError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SettingsError::InvalidFieldPath { value });
        }
        let mut segments = Vec::new();
        for raw in value.split('.') {
            if raw.is_empty() || (raw.contains('*') && raw != "*") {
                return Err(SettingsError::InvalidFieldPath { value });
            }
            segments.push(if raw == "*" {
                PathSegment::Wildcard
            } else {
                PathSegment::Key(raw.to_owned())
            });
        }
        Ok(Self(segments))
    }

    fn covers(&self, steps: &[Step<'_>]) -> bool {
        if self.0.len() > steps.len() {
            return false;
        }
        self.0
            .iter()
            .zip(steps)
            .all(|(segment, step)| match (segment, step) {
                (PathSegment::Wildcard, _) => true,
                (PathSegment::Key(key), Step::Key(actual)) => key == actual,
                (PathSegment::Key(_), Step::Index(_) | Step::Any) => false,
            })
    }
}

impl std::fmt::Display for SettingsFieldPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for segment in &self.0 {
            if !first {
                formatter.write_str(".")?;
            }
            first = false;
            match segment {
                PathSegment::Key(key) => formatter.write_str(key)?,
                PathSegment::Wildcard => formatter.write_str("*")?,
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for SettingsFieldPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "SettingsFieldPath({self})")
    }
}

/// One concrete position reached while walking a document.
#[derive(Clone, Copy)]
enum Step<'a> {
    Key(&'a str),
    Index(usize),
    /// A generic schema position standing for every key of a map or array.
    Any,
}

/// Owner-declared per-path classification for one namespace.
#[derive(Debug, Clone, Default)]
pub(crate) struct FieldRoles {
    secret: Vec<SettingsFieldPath>,
    public: Vec<SettingsFieldPath>,
}

impl FieldRoles {
    pub(crate) fn declare_secret(&mut self, path: SettingsFieldPath) {
        self.secret.push(path);
    }

    pub(crate) fn declare_public(&mut self, path: SettingsFieldPath) {
        self.public.push(path);
    }

    /// The first path declared both secret and public, if any.
    pub(crate) fn contradiction(&self) -> Option<&SettingsFieldPath> {
        self.secret.iter().find(|path| self.public.contains(path))
    }

    fn is_secret(&self, steps: &[Step<'_>]) -> bool {
        self.secret.iter().any(|path| path.covers(steps))
    }

    fn is_public(&self, steps: &[Step<'_>]) -> bool {
        self.public.iter().any(|path| path.covers(steps))
    }
}

/// Every layer one namespace would project across an inspection boundary.
pub(crate) struct ExposureInputs<'a> {
    pub(crate) schema: &'a Value,
    pub(crate) defaults: &'a Value,
    pub(crate) base: Option<&'a Value>,
    pub(crate) user: Option<&'a Value>,
    pub(crate) project: Option<&'a Value>,
    pub(crate) override_layer: Option<&'a Value>,
    pub(crate) managed: Option<&'a Value>,
    pub(crate) resolved: &'a Value,
}

/// Verified, fully redacted projection of one wire-exposed namespace.
///
/// A projection exists only where the exposure proof succeeded, so a caller
/// holding one needs no further redaction step of its own.
#[derive(Debug, Clone)]
pub struct SettingsWireProjection {
    schema: Value,
    defaults: Value,
    base: Option<Value>,
    user: Option<Value>,
    project: Option<Value>,
    override_layer: Option<Value>,
    managed: Option<Value>,
    resolved: Value,
    redacted_paths: Vec<String>,
}

impl SettingsWireProjection {
    /// Owner-declared schema metadata.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// Redacted schema-default layer.
    #[must_use]
    pub fn defaults(&self) -> &Value {
        &self.defaults
    }

    /// Redacted composition base layer.
    #[must_use]
    pub fn base(&self) -> Option<&Value> {
        self.base.as_ref()
    }

    /// Redacted user layer.
    #[must_use]
    pub fn user(&self) -> Option<&Value> {
        self.user.as_ref()
    }

    /// Redacted trusted-project layer.
    #[must_use]
    pub fn project(&self) -> Option<&Value> {
        self.project.as_ref()
    }

    /// Redacted administrator-managed layer.
    #[must_use]
    pub fn managed(&self) -> Option<&Value> {
        self.managed.as_ref()
    }

    /// Redacted ephemeral command-line override layer.
    #[must_use]
    pub fn override_layer(&self) -> Option<&Value> {
        self.override_layer.as_ref()
    }

    /// Redacted fully resolved value.
    #[must_use]
    pub fn resolved(&self) -> &Value {
        &self.resolved
    }

    /// Every concrete path this projection replaced, in path order.
    #[must_use]
    pub fn redacted_paths(&self) -> &[String] {
        &self.redacted_paths
    }
}

/// Prove that every projected path of one namespace is safe, and project it.
///
/// # Errors
/// The rendered path and the closed fault of the first unprovable position.
pub(crate) fn verify_and_project(
    inputs: &ExposureInputs<'_>,
    roles: &FieldRoles,
) -> Result<SettingsWireProjection, (String, WireExposureFault)> {
    verify_schema_literals(inputs.schema, &mut vec![Step::Key("schema")])?;
    verify_schema_property_names(inputs.schema, roles, &mut Vec::new(), 0)?;

    let mut redacted = BTreeSet::new();
    let mut project = |value: &Value| verify_layer(value, roles, &mut redacted);
    let schema = inputs.schema.clone();
    let defaults = project(inputs.defaults)?;
    let base = inputs.base.map(&mut project).transpose()?;
    let user = inputs.user.map(&mut project).transpose()?;
    let project_layer = inputs.project.map(&mut project).transpose()?;
    let override_layer = inputs.override_layer.map(&mut project).transpose()?;
    let managed = inputs.managed.map(&mut project).transpose()?;
    let resolved = project(inputs.resolved)?;
    Ok(SettingsWireProjection {
        schema,
        defaults,
        base,
        user,
        project: project_layer,
        override_layer,
        managed,
        resolved,
        redacted_paths: redacted.into_iter().collect(),
    })
}

/// Replace every provably or plausibly secret position for a rendered form.
///
/// This never fails: diagnostics must stay printable. It is the always-on
/// screen behind `Debug`, and it is strictly weaker than the exposure proof
/// because it can only remove, never refuse.
pub(crate) fn redact_for_debug(value: &Value, roles: &FieldRoles) -> Value {
    redact_node(value, roles, &mut Vec::new())
}

/// Remove recognized credential material from an owner-supplied message.
///
/// Defence in depth for text this crate does not author. It cannot prove a
/// message is clean; the exposure proof and the closed fault set are what
/// guarantee this crate's own errors carry no values.
pub(crate) fn scrub_message(message: String) -> String {
    if message.contains("-----BEGIN") && message.contains("PRIVATE KEY") {
        return REDACTED_PLACEHOLDER.to_owned();
    }
    let mut scrubbed = String::with_capacity(message.len());
    let mut token = String::new();
    for character in message.chars() {
        if is_token_character(character) {
            token.push(character);
            continue;
        }
        flush_token(&mut token, &mut scrubbed);
        scrubbed.push(character);
    }
    flush_token(&mut token, &mut scrubbed);
    scrubbed
}

fn flush_token(token: &mut String, scrubbed: &mut String) {
    if token.is_empty() {
        return;
    }
    if is_credential_material(token) {
        scrubbed.push_str(REDACTED_PLACEHOLDER);
    } else {
        scrubbed.push_str(token);
    }
    token.clear();
}

const fn is_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '+' | '/' | '=')
}

fn verify_layer(
    value: &Value,
    roles: &FieldRoles,
    redacted: &mut BTreeSet<String>,
) -> Result<Value, (String, WireExposureFault)> {
    verify_node(value, roles, &mut Vec::new(), redacted)
}

fn verify_node<'a>(
    value: &'a Value,
    roles: &FieldRoles,
    steps: &mut Vec<Step<'a>>,
    redacted: &mut BTreeSet<String>,
) -> Result<Value, (String, WireExposureFault)> {
    if roles.is_secret(steps) {
        redacted.insert(render_path(steps));
        return Ok(Value::String(REDACTED_PLACEHOLDER.to_owned()));
    }
    match value {
        Value::Object(map) => {
            let mut projected = Map::new();
            for (key, child) in map {
                if is_credential_material(key) {
                    steps.push(Step::Key(key));
                    let path = render_path(steps);
                    steps.pop();
                    return Err((path, WireExposureFault::CredentialMaterial));
                }
                steps.push(Step::Key(key));
                let screened = screen_key(key, roles, steps);
                let outcome = match screened {
                    Err(fault) => Err((render_path(steps), fault)),
                    Ok(()) => verify_node(child, roles, steps, redacted),
                };
                steps.pop();
                projected.insert(key.clone(), outcome?);
            }
            Ok(Value::Object(projected))
        }
        Value::Array(items) => {
            let mut projected = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                steps.push(Step::Index(index));
                let outcome = verify_node(item, roles, steps, redacted);
                steps.pop();
                projected.push(outcome?);
            }
            Ok(Value::Array(projected))
        }
        Value::String(text) if is_credential_material(text) => {
            Err((render_path(steps), WireExposureFault::CredentialMaterial))
        }
        other => Ok(other.clone()),
    }
}

fn screen_key(key: &str, roles: &FieldRoles, steps: &[Step<'_>]) -> Result<(), WireExposureFault> {
    if roles.is_secret(steps) || roles.is_public(steps) || !is_secret_shaped_key(key) {
        return Ok(());
    }
    Err(WireExposureFault::SecretShapedKey)
}

/// Screen the property names a schema declares, at their data paths.
///
/// The reader understands only the JSON-Schema shapes that name a child
/// position. An unrecognized shape yields no extra obligation, which is
/// sound because this walk can only add faults.
fn verify_schema_property_names<'a>(
    node: &'a Value,
    roles: &FieldRoles,
    steps: &mut Vec<Step<'a>>,
    depth: usize,
) -> Result<(), (String, WireExposureFault)> {
    if depth >= MAX_SCHEMA_DEPTH {
        return Ok(());
    }
    let Some(object) = node.as_object() else {
        return Ok(());
    };
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            steps.push(Step::Key(name));
            let outcome = match screen_key(name, roles, steps) {
                Err(fault) => Err((render_path(steps), fault)),
                Ok(()) => verify_schema_property_names(child, roles, steps, depth + 1),
            };
            steps.pop();
            outcome?;
        }
    }
    for keyword in ["additionalProperties", "items", "contains"] {
        if let Some(child) = object.get(keyword).filter(|child| child.is_object()) {
            steps.push(Step::Any);
            let outcome = verify_schema_property_names(child, roles, steps, depth + 1);
            steps.pop();
            outcome?;
        }
    }
    if let Some(patterns) = object.get("patternProperties").and_then(Value::as_object) {
        for child in patterns.values() {
            steps.push(Step::Any);
            let outcome = verify_schema_property_names(child, roles, steps, depth + 1);
            steps.pop();
            outcome?;
        }
    }
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = object.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                verify_schema_property_names(branch, roles, steps, depth + 1)?;
            }
        }
    }
    Ok(())
}

/// Schema metadata is projected verbatim, so no literal inside it may be
/// credential material. There is no legitimate reason for one to be there,
/// so no owner role discharges this.
fn verify_schema_literals<'a>(
    node: &'a Value,
    steps: &mut Vec<Step<'a>>,
) -> Result<(), (String, WireExposureFault)> {
    match node {
        Value::Object(map) => {
            for (key, child) in map {
                steps.push(Step::Key(key));
                let outcome = if is_credential_material(key) {
                    Err((render_path(steps), WireExposureFault::CredentialMaterial))
                } else {
                    verify_schema_literals(child, steps)
                };
                steps.pop();
                outcome?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                steps.push(Step::Index(index));
                let outcome = verify_schema_literals(item, steps);
                steps.pop();
                outcome?;
            }
            Ok(())
        }
        Value::String(text) if is_credential_material(text) => {
            Err((render_path(steps), WireExposureFault::CredentialMaterial))
        }
        _ => Ok(()),
    }
}

fn redact_node<'a>(value: &'a Value, roles: &FieldRoles, steps: &mut Vec<Step<'a>>) -> Value {
    if roles.is_secret(steps) {
        return Value::String(REDACTED_PLACEHOLDER.to_owned());
    }
    match value {
        Value::Object(map) => {
            let mut rendered = Map::new();
            for (key, child) in map {
                if is_credential_material(key) {
                    rendered.insert(
                        REDACTED_PLACEHOLDER.to_owned(),
                        Value::String(REDACTED_PLACEHOLDER.to_owned()),
                    );
                    continue;
                }
                steps.push(Step::Key(key));
                let child = if screen_key(key, roles, steps).is_err() {
                    Value::String(REDACTED_PLACEHOLDER.to_owned())
                } else {
                    redact_node(child, roles, steps)
                };
                steps.pop();
                rendered.insert(key.clone(), child);
            }
            Value::Object(rendered)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    steps.push(Step::Index(index));
                    let item = redact_node(item, roles, steps);
                    steps.pop();
                    item
                })
                .collect(),
        ),
        Value::String(text) if is_credential_material(text) => {
            Value::String(REDACTED_PLACEHOLDER.to_owned())
        }
        other => other.clone(),
    }
}

/// Render one concrete position. A key that is itself credential material is
/// replaced, so reporting a path can never publish the secret it names.
fn render_path(steps: &[Step<'_>]) -> String {
    let mut rendered = String::new();
    for step in steps {
        match step {
            Step::Key(key) => {
                if !rendered.is_empty() {
                    rendered.push('.');
                }
                if is_credential_material(key) {
                    rendered.push_str(REDACTED_PLACEHOLDER);
                } else {
                    rendered.push_str(key);
                }
            }
            Step::Index(index) => rendered.push_str(&format!("[{index}]")),
            Step::Any => {
                if !rendered.is_empty() {
                    rendered.push('.');
                }
                rendered.push('*');
            }
        }
    }
    rendered
}

/// Leaf paths assigned by one layer, in path order.
pub(crate) fn leaf_paths(value: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_leaf_paths(value, &mut Vec::new(), &mut paths);
    paths
}

fn collect_leaf_paths<'a>(value: &'a Value, steps: &mut Vec<Step<'a>>, paths: &mut Vec<String>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                steps.push(Step::Key(key));
                collect_leaf_paths(child, steps, paths);
                steps.pop();
            }
        }
        _ => paths.push(render_path(steps)),
    }
}

/// Whether two rendered paths are prefix-comparable, so one assignment would
/// shadow or be shadowed by the other.
pub(crate) fn paths_overlap(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let (shorter, longer) = if left.len() < right.len() {
        (left, right)
    } else {
        (right, left)
    };
    !shorter.is_empty()
        && longer.starts_with(shorter)
        && longer
            .get(shorter.len()..)
            .is_some_and(|rest| rest.starts_with(['.', '[']))
}

const SECRET_TOKENS: &[&str] = &[
    "secret",
    "secrets",
    "password",
    "passwords",
    "passwd",
    "pwd",
    "passphrase",
    "token",
    "credential",
    "credentials",
    "apikey",
    "apisecret",
    "accesskey",
    "accesstoken",
    "authtoken",
    "clientsecret",
    "privatekey",
    "refreshtoken",
    "bearer",
];

const SECRET_TOKEN_PAIRS: &[(&str, &str)] = &[
    ("api", "key"),
    ("api", "secret"),
    ("access", "key"),
    ("access", "token"),
    ("auth", "token"),
    ("client", "secret"),
    ("encryption", "key"),
    ("id", "token"),
    ("private", "key"),
    ("refresh", "token"),
    ("secret", "key"),
    ("service", "key"),
    ("session", "token"),
    ("signing", "key"),
];

/// Whether a key name states that its value is credential material.
fn is_secret_shaped_key(key: &str) -> bool {
    let tokens = tokenize(key);
    if tokens
        .iter()
        .any(|token| SECRET_TOKENS.contains(&token.as_str()))
    {
        return true;
    }
    tokens.windows(2).any(|window| match window {
        [first, second] => SECRET_TOKEN_PAIRS.contains(&(first.as_str(), second.as_str())),
        _ => false,
    })
}

/// Split a key into lowercase word tokens on separators and case changes.
fn tokenize(key: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for character in key.chars() {
        if !character.is_ascii_alphanumeric() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if character.is_ascii_uppercase() && previous_lower && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        previous_lower = character.is_ascii_lowercase() || character.is_ascii_digit();
        current.push(character.to_ascii_lowercase());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Issuer prefixes for widely published credential formats and the shortest
/// total length at which the prefix is evidence rather than coincidence.
const MATERIAL_PREFIXES: &[(&str, usize)] = &[
    ("sk-", 12),
    ("sk_", 12),
    ("pk_live_", 16),
    ("rk_live_", 16),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("github_pat_", 24),
    ("xoxb-", 20),
    ("xoxa-", 20),
    ("xoxp-", 20),
    ("xoxs-", 20),
    ("xoxr-", 20),
    ("glpat-", 20),
    ("npm_", 24),
    ("hf_", 24),
    ("dop_v1_", 24),
    ("ya29.", 24),
    ("AIza", 35),
    ("Bearer ", 27),
];

/// Whether a string matches a recognized credential format.
///
/// Structural recognition only: no entropy scoring, because a false positive
/// here refuses a whole namespace. Recognition is evidence of a secret, never
/// evidence of its absence, so it may deny but can never grant.
/// Screen free text for credential material.
///
/// Splits on characters that cannot occur inside a credential and screens each
/// token with the same recognizers the settings wire screen uses. There is
/// exactly one list of what a credential looks like, and it lives here: a
/// second copy that drifts is how a leak gets shipped.
///
/// This can only ever refuse. A token it does not recognize is not thereby
/// declared safe — callers that need proof of safety must withhold by default
/// and treat this as one screen among others.
///
/// # Errors
/// [`WireExposureFault::CredentialMaterial`] when any token matches a
/// recognized credential format.
pub fn screen_text_for_credentials(text: &str) -> Result<(), WireExposureFault> {
    let recognized = text
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
                )
        })
        .any(|token| is_credential_material(token.trim_matches(|c: char| c == ':' || c == '=')));
    if recognized {
        return Err(WireExposureFault::CredentialMaterial);
    }
    // A PEM block spans lines, so the token walk above cannot see it.
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return Err(WireExposureFault::CredentialMaterial);
    }
    Ok(())
}

/// True when a field name announces that its value is credential material.
///
/// Exposed for callers outside settings that must apply the same naming screen
/// — an artifact recording a live run screens its own metadata keys with this.
#[must_use]
pub fn names_credential_material(key: &str) -> bool {
    is_secret_shaped_key(key)
}

fn is_credential_material(value: &str) -> bool {
    if MATERIAL_PREFIXES
        .iter()
        .any(|(prefix, minimum)| value.len() >= *minimum && value.starts_with(prefix))
    {
        return true;
    }
    if value.contains("-----BEGIN") && value.contains("PRIVATE KEY") {
        return true;
    }
    if value.len() == 20
        && value.starts_with("AKIA")
        && value
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
    {
        return true;
    }
    is_json_web_token(value)
}

fn is_json_web_token(value: &str) -> bool {
    if !value.starts_with("eyJ") {
        return false;
    }
    let segments: Vec<&str> = value.split('.').collect();
    segments.len() == 3
        && segments.iter().all(|segment| {
            segment.len() >= 10
                && segment.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
}
