//! Insertion-ordered tool registry.

use std::sync::Arc;

use heycode_core::ToolSpec;
use heycode_exec::ObservationLog;

use crate::tool::Tool;

/// Why [`ToolRegistry::register`] refused a tool.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    /// A tool with the same name is already registered.
    #[error("tool \"{0}\" is already registered")]
    Duplicate(String),

    /// The tool's `parameters` is not a JSON-Schema object.
    ///
    /// The model contract requires an object schema; anything else would
    /// produce malformed tool declarations downstream.
    #[error("tool \"{0}\" must declare an object JSON Schema for parameters")]
    InvalidSchema(String),
    /// A compatibility alias is malformed or repeated.
    #[error("tool alias for \"{0}\" is invalid")]
    InvalidAlias(String),
    /// Late registry mutex was poisoned.
    #[error("tool registry is unavailable")]
    RegistryUnavailable,
}

/// Insertion-ordered registry of named tools.
///
/// Iteration order of [`ToolRegistry::names`] and [`ToolRegistry::specs`] is
/// registration order, which keeps tool lists stable for prompts and tests.
/// Every registry exposes the active filesystem provider's
/// [`ObservationLog`], so tests/diagnostics inspect the same records that
/// enforce mutations (see [`crate::builtin_tools`]).
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<(String, Arc<dyn Tool>)>,
    /// Late registrants (e.g. the agent's `task` tool) added after the
    /// registry was published as an immutable service.
    late: Arc<std::sync::Mutex<Vec<LateTool>>>,
    observations: ObservationLog,
    observation_filesystem: Option<heycode_exec::FileSystemService>,
}

struct LateTool {
    name: String,
    tool: Arc<dyn Tool>,
    token: Arc<()>,
}

/// RAII ownership of one token-matched late tool registration.
///
/// Dropping or explicitly unregistering this handle removes only its exact
/// registration. A stale handle cannot remove a later same-name replacement.
pub struct OwnedToolRegistration {
    late: std::sync::Weak<std::sync::Mutex<Vec<LateTool>>>,
    name: String,
    token: Arc<()>,
}

impl OwnedToolRegistration {
    /// Registered model-facing tool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Remove this exact registration if it is still current.
    ///
    /// This operation is idempotent. The handle deliberately remains armed;
    /// a later Drop repeats the token check and cannot affect a replacement.
    pub fn unregister(&self) {
        let Some(late) = self.late.upgrade() else {
            return;
        };
        let Ok(mut tools) = late.lock() else {
            return;
        };
        tools.retain(|registered| {
            registered.name != self.name || !Arc::ptr_eq(&registered.token, &self.token)
        });
    }
}

impl Drop for OwnedToolRegistration {
    fn drop(&mut self) {
        self.unregister();
    }
}

impl ToolRegistry {
    /// An empty registry with a fresh observation log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register after publication. Duplicate names fail loud, mirroring
    /// [`Self::register`].
    ///
    /// # Errors
    /// Duplicate names, invalid schemas or poisoned late state.
    pub fn register_shared(&self, tool: Arc<dyn Tool>) -> Result<(), RegisterError> {
        let _identity = self.insert_late(tool)?;
        Ok(())
    }

    /// Register a token-owned late tool.
    ///
    /// The returned handle is the sole lifecycle owner. Dropping it removes
    /// only the matching row, making this suitable for plugin generations and
    /// rollback-safe candidate assembly.
    ///
    /// # Errors
    /// Duplicate names, invalid schemas or poisoned registry state fail before
    /// publication.
    pub fn register_owned(
        &self,
        tool: Arc<dyn Tool>,
    ) -> Result<OwnedToolRegistration, RegisterError> {
        let (name, token) = self.insert_late(tool)?;
        Ok(OwnedToolRegistration {
            late: Arc::downgrade(&self.late),
            name,
            token,
        })
    }

    fn insert_late(&self, tool: Arc<dyn Tool>) -> Result<(String, Arc<()>), RegisterError> {
        let spec = tool.spec();
        let name = spec.name;
        if !spec.parameters.is_object() {
            return Err(RegisterError::InvalidSchema(name));
        }
        let mut late = self
            .late
            .lock()
            .map_err(|_| RegisterError::RegistryUnavailable)?;
        validate_names(
            &name,
            tool.as_ref(),
            self.tools
                .iter()
                .map(|(name, tool)| (name.as_str(), tool.as_ref()))
                .chain(
                    late.iter()
                        .map(|row| (row.name.as_str(), row.tool.as_ref())),
                ),
        )?;
        let token = Arc::new(());
        late.push(LateTool {
            name: name.clone(),
            tool,
            token: token.clone(),
        });
        Ok((name, token))
    }

    /// An empty registry exposing `observations` from the active filesystem
    /// provider.
    #[must_use]
    pub fn with_observations(observations: ObservationLog) -> Self {
        Self {
            tools: Vec::new(),
            late: Arc::new(std::sync::Mutex::new(Vec::new())),
            observations,
            observation_filesystem: None,
        }
    }

    /// Keep diagnostics aligned with a filesystem whose provider generation can
    /// change. Fresh generation logs deliberately invalidate prior observations.
    #[must_use]
    pub fn with_filesystem_observations(filesystem: heycode_exec::FileSystemService) -> Self {
        let mut registry = Self::with_observations(filesystem.observations());
        registry.observation_filesystem = Some(filesystem);
        registry
    }

    /// Register `tool`; duplicate names fail loud.
    ///
    /// # Errors
    /// - [`RegisterError::Duplicate`] when a tool with the same spec name exists
    /// - [`RegisterError::InvalidSchema`] when `spec().parameters` is not an object
    /// - [`RegisterError::RegistryUnavailable`] when late state is poisoned
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), RegisterError> {
        let spec = tool.spec();
        if !spec.parameters.is_object() {
            return Err(RegisterError::InvalidSchema(spec.name));
        }
        let late = self
            .late
            .lock()
            .map_err(|_| RegisterError::RegistryUnavailable)?;
        validate_names(
            &spec.name,
            tool.as_ref(),
            self.tools
                .iter()
                .map(|(name, tool)| (name.as_str(), tool.as_ref()))
                .chain(
                    late.iter()
                        .map(|row| (row.name.as_str(), row.tool.as_ref())),
                ),
        )?;
        drop(late);
        self.tools.push((spec.name, tool));
        Ok(())
    }

    /// Look up a canonical tool or one of its explicit historical aliases.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some((_, tool)) = self
            .tools
            .iter()
            .find(|(n, tool)| n == name || tool.aliases().contains(&name))
        {
            return Some(Arc::clone(tool));
        }
        self.late
            .lock()
            .ok()?
            .iter()
            .find(|registered| registered.name == name || registered.tool.aliases().contains(&name))
            .map(|registered| Arc::clone(&registered.tool))
    }

    /// Every tool's model-facing spec, in registration order.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut tools: Vec<_> = self
            .tools
            .iter()
            .map(|(name, tool)| (name.clone(), tool.clone()))
            .collect();
        if let Ok(late) = self.late.lock() {
            tools.extend(late.iter().map(|row| (row.name.clone(), row.tool.clone())));
        }
        let names: std::collections::HashSet<_> =
            tools.iter().map(|(name, _)| name.as_str()).collect();
        tools
            .iter()
            .filter(|(_, tool)| {
                tool.model_replacement()
                    .is_none_or(|name| !names.contains(name))
            })
            .map(|(_, tool)| tool.spec())
            .collect()
    }

    /// Advertised names for model prompts; compatibility dispatch uses `names`.
    #[must_use]
    pub fn advertised_names(&self) -> Vec<String> {
        self.specs().into_iter().map(|spec| spec.name).collect()
    }

    /// Every registered name, in registration order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.iter().map(|(name, _)| name.clone()).collect();
        if let Ok(late) = self.late.lock() {
            names.extend(late.iter().map(|registered| registered.name.clone()));
        }
        names
    }

    /// Clone handle to this registry's observation log; tools and tests share
    /// it to enforce read-before-mutate.
    #[must_use]
    pub fn observations(&self) -> ObservationLog {
        self.observation_filesystem.as_ref().map_or_else(
            || self.observations.clone(),
            heycode_exec::FileSystemService::observations,
        )
    }
}

fn validate_names<'a>(
    name: &str,
    tool: &dyn Tool,
    existing: impl Iterator<Item = (&'a str, &'a dyn Tool)>,
) -> Result<(), RegisterError> {
    let mut names = std::collections::BTreeSet::from([name]);
    for alias in tool.aliases() {
        if alias.is_empty()
            || alias.len() > 128
            || !alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            || !names.insert(alias)
        {
            return Err(RegisterError::InvalidAlias(name.into()));
        }
    }
    for (registered, other) in existing {
        if let Some(conflict) = std::iter::once(registered)
            .chain(other.aliases().iter().copied())
            .find(|candidate| names.contains(candidate))
        {
            return Err(RegisterError::Duplicate(conflict.into()));
        }
    }
    Ok(())
}

impl ToolRegistry {
    /// Clone the current tool set while rebinding local execution to an authorized child workspace.
    pub fn for_workspace(
        &self,
        filesystem: &heycode_exec::FileSystemService,
        shell: &heycode_exec::ShellService,
    ) -> Result<Self, RegisterError> {
        let registry = Self::with_filesystem_observations(filesystem.clone());
        for name in self.names() {
            if let Some(tool) = self.get(&name) {
                registry
                    .register_shared(tool.rebind_workspace(filesystem, shell).unwrap_or(tool))?;
            }
        }
        Ok(registry)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::tool::ToolError;

    /// Minimal tool double whose spec name is fully controlled by the caller.
    struct NamedTool(heycode_core::ToolSpec);

    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn spec(&self) -> heycode_core::ToolSpec {
            self.0.clone()
        }

        async fn run(
            &self,
            args: serde_json::Value,
            _cx: &crate::tool::ToolCtx,
        ) -> Result<serde_json::Value, ToolError> {
            Ok(args)
        }
    }

    fn echo_tool_for_tests(spec: heycode_core::ToolSpec) -> Arc<dyn Tool> {
        Arc::new(NamedTool(spec))
    }

    fn spec_named(name: &str) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: name.to_owned(),
            description: "test".to_owned(),
            parameters: json!({"type": "object"}),
        }
    }

    struct CompatibilityTool;
    #[async_trait::async_trait]
    impl Tool for CompatibilityTool {
        fn spec(&self) -> ToolSpec {
            spec_named("old_control")
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["older_control"]
        }
        fn model_replacement(&self) -> Option<&'static str> {
            Some("new_control")
        }
        async fn run(
            &self,
            args: serde_json::Value,
            _: &crate::ToolCtx,
        ) -> Result<serde_json::Value, ToolError> {
            Ok(args)
        }
    }

    #[test]
    fn model_replacement_hides_only_opted_in_schema_while_replacement_is_registered() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CompatibilityTool)).unwrap();
        registry
            .register(echo_tool_for_tests(spec_named("third_party_control")))
            .unwrap();
        assert_eq!(
            registry.advertised_names(),
            ["old_control", "third_party_control"]
        );
        let replacement = registry
            .register_owned(echo_tool_for_tests(spec_named("new_control")))
            .unwrap();
        assert_eq!(
            registry.advertised_names(),
            ["third_party_control", "new_control"]
        );
        assert_eq!(
            registry.names(),
            ["old_control", "third_party_control", "new_control"]
        );
        assert!(registry.get("old_control").is_some());
        assert!(registry.get("older_control").is_some());
        drop(replacement);
        assert_eq!(
            registry.advertised_names(),
            ["old_control", "third_party_control"]
        );
        assert!(registry.get("older_control").is_some());
    }

    #[test]
    fn register_preserves_insertion_order_and_lookups() {
        let mut reg = ToolRegistry::new();
        reg.register(echo_tool_for_tests(spec_named("alpha")))
            .unwrap();
        reg.register(echo_tool_for_tests(spec_named("beta")))
            .unwrap();
        assert_eq!(reg.names(), vec!["alpha".to_owned(), "beta".to_owned()]);
        assert_eq!(reg.specs().len(), 2);
        assert_eq!(reg.specs()[1].name, "beta");
        assert!(reg.get("alpha").is_some());
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn register_rejects_duplicate_names_loudly() {
        let mut reg = ToolRegistry::new();
        reg.register(echo_tool_for_tests(spec_named("dup")))
            .unwrap();
        let err = reg
            .register(echo_tool_for_tests(spec_named("dup")))
            .unwrap_err();
        match err {
            RegisterError::Duplicate(name) => assert_eq!(name, "dup"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn register_rejects_non_object_schema() {
        let mut bad = spec_named("bad");
        bad.parameters = json!([1, 2]);
        let mut reg = ToolRegistry::new();
        let err = reg.register(echo_tool_for_tests(bad)).unwrap_err();
        match err {
            RegisterError::InvalidSchema(name) => assert_eq!(name, "bad"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn owned_late_registration_disappears_when_its_handle_drops() {
        let registry = Arc::new(ToolRegistry::new());
        let handle = registry
            .register_owned(echo_tool_for_tests(spec_named("owned")))
            .unwrap();
        assert_eq!(handle.name(), "owned");
        assert!(registry.get("owned").is_some());
        assert_eq!(registry.names(), ["owned"]);

        drop(handle);
        assert!(registry.get("owned").is_none());
        assert!(registry.names().is_empty());
    }

    #[test]
    fn stale_owned_handle_cannot_remove_a_replacement() {
        let registry = Arc::new(ToolRegistry::new());
        let stale = registry
            .register_owned(echo_tool_for_tests(spec_named("replaceable")))
            .unwrap();
        stale.unregister();
        let replacement = registry
            .register_owned(echo_tool_for_tests(spec_named("replaceable")))
            .unwrap();

        drop(stale);
        assert!(registry.get("replaceable").is_some());
        drop(replacement);
        assert!(registry.get("replaceable").is_none());
    }

    #[test]
    fn observation_log_round_trips_through_canonical_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        let log = ObservationLog::default();
        assert!(log.is_empty());
        // Mark via a differently spelled path; contains must still match.
        let dotted = dir.path().join(".").join("f.txt");
        log.mark(&dotted);
        assert!(!log.is_empty());
        assert_eq!(log.len(), 1);
        assert!(log.contains(&file));
        assert!(!log.contains(&dir.path().join("other.txt")));
        assert_eq!(log.snapshot().len(), 1);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod alias_tests {
    use super::*;
    struct Aliased;
    #[async_trait::async_trait]
    impl Tool for Aliased {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "agent".into(),
                description: "Delegate".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["task"]
        }
        async fn run(
            &self,
            args: serde_json::Value,
            _: &crate::ToolCtx,
        ) -> Result<serde_json::Value, crate::ToolError> {
            Ok(args)
        }
    }
    struct OldName;
    #[async_trait::async_trait]
    impl Tool for OldName {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "task".into(),
                description: "Conflicting old name".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        }
        async fn run(
            &self,
            args: serde_json::Value,
            _: &crate::ToolCtx,
        ) -> Result<serde_json::Value, crate::ToolError> {
            Ok(args)
        }
    }
    #[test]
    fn aliases_are_hidden_collisions_are_atomic_and_disposal_removes_both_names() {
        let registry = ToolRegistry::new();
        let owner = registry.register_owned(Arc::new(Aliased)).unwrap();
        assert_eq!(registry.names(), vec!["agent"]);
        assert_eq!(registry.specs().len(), 1);
        assert!(Arc::ptr_eq(
            &registry.get("agent").unwrap(),
            &registry.get("task").unwrap()
        ));
        assert!(matches!(
            registry.register_shared(Arc::new(OldName)),
            Err(RegisterError::Duplicate(_))
        ));
        drop(owner);
        assert!(registry.get("agent").is_none() && registry.get("task").is_none());
        registry.register_shared(Arc::new(OldName)).unwrap();
        assert!(matches!(
            registry.register_owned(Arc::new(Aliased)),
            Err(RegisterError::Duplicate(_))
        ));
        assert!(registry.get("agent").is_none());
    }
    struct DenyCanonical;
    #[async_trait::async_trait]
    impl heycode_core::Layer<crate::PreToolDecision> for DenyCanonical {
        async fn handle(
            &self,
            input: &mut crate::PreToolDecision,
            _: heycode_core::Next<'_, crate::PreToolDecision>,
        ) -> anyhow::Result<()> {
            assert_eq!(input.call.name, "agent");
            input.verdict = crate::Verdict::Deny {
                reason: "canonical agent policy".into(),
            };
            Ok(())
        }
    }
    #[tokio::test]
    async fn historical_alias_is_guarded_under_the_canonical_identity() {
        let registry = ToolRegistry::new();
        registry.register_shared(Arc::new(Aliased)).unwrap();
        let mut pre = heycode_core::Waterfall::new();
        pre.push(DenyCanonical);
        let result = crate::execute_tool(
            &registry,
            &pre,
            crate::ToolCallInput {
                name: "task".into(),
                args: serde_json::json!({}),
            },
            &crate::ToolCtx::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            result.denied_reason.as_deref(),
            Some("canonical agent policy")
        );
    }
}
