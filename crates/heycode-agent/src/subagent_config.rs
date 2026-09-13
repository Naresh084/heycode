//! Immutable, narrowing policy resolved before a custom child starts.

use serde::{Deserialize, Serialize};

/// Preset execution controls. Missing fields inherit the native runner defaults.
/// These controls never grant authority absent from the parent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubagentConfig {
    /// Optional registered inference provider; requires an explicit model.
    pub inference_provider: Option<String>,
    /// Exact inference model id (on the inherited inference provider).
    pub model: Option<String>,
    /// Adapter-validated reasoning effort id.
    pub effort: Option<String>,
    /// Exact client tool names; an empty list allows no tools.
    pub tools: Option<Vec<String>>,
    /// Exact client tool names removed from the inherited set.
    pub denied_tools: Vec<String>,
    /// Inherit parent approval or additionally require read-only tools.
    pub permissions: ChildPermissions,
    /// Exact skill names allowed through load_skill; empty disables it.
    pub skills: Option<Vec<String>>,
    /// Exact MCP server ids; empty disables all MCP tools.
    pub mcp_servers: Option<Vec<String>>,
    /// Maximum inference steps across the child's entire retained lifetime.
    pub max_turns: Option<u32>,
    /// Conversation history or a persistent per-preset memory scope.
    pub memory: ChildMemory,
    /// Filesystem isolation requested from the native lifecycle owner.
    pub isolation: ChildIsolation,
    /// Default for task(background), overridable at invocation.
    pub background: bool,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            inference_provider: None,
            model: None,
            effort: None,
            tools: None,
            denied_tools: Vec::new(),
            permissions: ChildPermissions::default(),
            skills: None,
            mcp_servers: None,
            max_turns: None,
            memory: ChildMemory::default(),
            isolation: ChildIsolation::default(),
            background: true,
        }
    }
}

/// Current native execution values plus the immutable preset policy. Parent
/// guards and live approval still constrain every call independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedSubagentConfig {
    /// Actual registered inference provider.
    pub inference_provider: String,
    /// Actual selected model.
    pub model: String,
    /// Actual adapter effort, if explicit.
    pub effort: Option<String>,
    /// Client tool names in this child's current filtered registry.
    pub tools: Vec<String>,
    /// Immutable declared policy; absent overrides were inherited at creation.
    pub policy: SubagentConfig,
}

/// Additional permission ceiling; parent approval always applies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildPermissions {
    /// Preserve parent policy.
    #[default]
    Inherit,
    /// Admit only the explicit local read-only tool set.
    ReadOnly,
    /// Require a fresh human decision; headless calls are denied.
    Default,
    /// Child-scoped remembered grants, while parent approval still applies.
    AcceptedEdits,
    /// No extra prompting; parent policy remains the authority ceiling.
    FullAccess,
    /// Deny all tool execution.
    Deny,
}

/// Supported memory policy. Durable sessions are always retained for audit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildMemory {
    /// Retain this child's conversation only.
    #[default]
    Session,
    /// Private memory shared across projects under the native state root.
    User,
    /// Project memory, suitable for version control.
    Project,
    /// Project-specific private memory under the native state root.
    Local,
}

/// Native filesystem isolation contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildIsolation {
    /// Use the parent's working directory and guards.
    #[default]
    Shared,
    /// An isolated native worktree; requires a composed worktree manager.
    Worktree,
}

impl SubagentConfig {
    /// Validate bounds before publishing or executing a preset.
    ///
    /// # Errors
    /// Blank, oversized, duplicate names or zero/oversized limits.
    pub fn validate(&self) -> Result<(), &'static str> {
        fn name(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 256
                && value.trim() == value
                && !value.chars().any(char::is_control)
        }
        for value in [&self.inference_provider, &self.model, &self.effort]
            .into_iter()
            .flatten()
        {
            if !name(value) {
                return Err("provider/model/effort must be bounded nonblank ids");
            }
        }
        if self.inference_provider.is_some() && self.model.is_none() {
            return Err("inference_provider requires an explicit model");
        }
        for list in [
            self.tools.as_ref(),
            Some(&self.denied_tools),
            self.skills.as_ref(),
            self.mcp_servers.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            let unique: std::collections::BTreeSet<_> = list.iter().collect();
            if list.len() > 256 || unique.len() != list.len() || !list.iter().all(|v| name(v)) {
                return Err("policy lists require at most 256 unique bounded names");
            }
        }
        if self.max_turns.is_some_and(|n| n == 0 || n > 10000) {
            return Err("max_turns must be between 1 and 10000");
        }
        Ok(())
    }

    /// Whether client/hosted tool authority needs narrowing.
    #[must_use]
    pub fn restricts_tools(&self) -> bool {
        self.tools.is_some()
            || !self.denied_tools.is_empty()
            || self.skills.is_some()
            || self.mcp_servers.is_some()
            || !matches!(
                self.permissions,
                ChildPermissions::Inherit | ChildPermissions::FullAccess
            )
    }

    /// Exact schema and execution filter. Restrictive children cannot delegate
    /// to shared tools whose captured services would escape this ceiling.
    #[must_use]
    pub fn allows_tool(&self, name: &str) -> bool {
        let name = canonical_agent_tool(name);
        if self.permissions == ChildPermissions::Deny {
            return false;
        }
        if self
            .denied_tools
            .iter()
            .any(|n| canonical_agent_tool(n) == name)
            || self
                .tools
                .as_ref()
                .is_some_and(|list| !list.iter().any(|n| canonical_agent_tool(n) == name))
        {
            return false;
        }
        if self.restricts_tools()
            && matches!(
                name,
                "agent"
                    | "agent_control"
                    | "job_control"
                    | "workflow"
                    | "team"
                    | "background_shell"
                    | "background_terminal"
                    | "schedule_create"
                    | "goal"
            )
        {
            return false;
        }
        // Local findings append to the review owner, so read-only children must
        // opt in explicitly; this does not grant filesystem/process mutation.
        let explicit_findings = name == "report_findings"
            && self
                .tools
                .as_ref()
                .is_some_and(|tools| tools.iter().any(|tool| tool == name));
        if self.permissions == ChildPermissions::ReadOnly
            && !explicit_findings
            && !matches!(
                name,
                "read"
                    | "read_many"
                    | "glob"
                    | "grep"
                    | "load_skill"
                    | "agent_memory_read"
                    | "lsp_servers"
                    | "lsp_definition"
                    | "lsp_references"
                    | "lsp_diagnostics"
                    | "ask_user_question"
                    | "ask_user_question_async"
                    | "send_message"
            )
        {
            return false;
        }
        if name == "load_skill" && self.skills.as_ref().is_some_and(Vec::is_empty) {
            return false;
        }
        if name.starts_with("mcp__")
            && let Some(servers) = &self.mcp_servers
        {
            return servers
                .iter()
                .any(|server| name.starts_with(&format!("mcp__{server}__")));
        }
        true
    }

    /// Argument-level policy for skill access, in addition to schema filtering.
    #[must_use]
    pub fn allows_call(&self, name: &str, arguments: &serde_json::Value) -> bool {
        self.allows_tool(name)
            && (name != "load_skill"
                || self.skills.as_ref().is_none_or(|skills| {
                    arguments
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|name| skills.iter().any(|allowed| allowed == name))
                }))
    }
}

/// Registry-level policy wrapper. Code-mode and other guarded dispatchers can
/// reuse a child's ToolRegistry without bypassing argument-level restrictions.
pub(crate) struct ScopedChildTool {
    pub(crate) inner: std::sync::Arc<dyn heycode_tools::Tool>,
    pub(crate) policy: std::sync::Arc<SubagentConfig>,
}

#[async_trait::async_trait]
impl heycode_tools::Tool for ScopedChildTool {
    fn aliases(&self) -> &'static [&'static str] {
        self.inner.aliases()
    }
    fn model_replacement(&self) -> Option<&'static str> {
        self.inner.model_replacement()
    }
    fn spec(&self) -> heycode_core::ToolSpec {
        let mut spec = self.inner.spec();
        if spec.name == "load_skill"
            && let Some(skills) = &self.policy.skills
            && let Some(property) = spec
                .parameters
                .get_mut("properties")
                .and_then(|properties| properties.get_mut("name"))
                .and_then(serde_json::Value::as_object_mut)
        {
            property.insert("enum".to_owned(), serde_json::json!(skills));
        }
        spec
    }
    fn effect(&self) -> heycode_tools::ToolEffect {
        self.inner.effect()
    }
    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        self.inner.untrusted_content()
    }
    async fn run(
        &self,
        args: serde_json::Value,
        cx: &heycode_tools::ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        if !self.policy.allows_call(&self.inner.spec().name, &args) {
            return Err(heycode_tools::ToolError::new(
                "custom agent policy denies this tool call",
            ));
        }
        self.inner.run(args, cx).await
    }
    async fn run_output(
        &self,
        args: serde_json::Value,
        cx: &heycode_tools::ToolCtx,
    ) -> Result<heycode_tools::ToolOutput, heycode_tools::ToolError> {
        if !self.policy.allows_call(&self.inner.spec().name, &args) {
            return Err(heycode_tools::ToolError::new(
                "custom agent policy denies this tool call",
            ));
        }
        self.inner.run_output(args, cx).await
    }
}

fn canonical_agent_tool(name: &str) -> &str {
    match name {
        "task" => "agent",
        "list_tasks" => "list_agents",
        other => other,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    struct CountSkill(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl heycode_tools::Tool for CountSkill {
        fn spec(&self) -> heycode_core::ToolSpec {
            heycode_core::ToolSpec {
                name: "load_skill".into(),
                description: "test".into(),
                parameters: serde_json::json!({"type":"object","properties":{"name":{"type":"string"}}}),
            }
        }
        fn effect(&self) -> heycode_tools::ToolEffect {
            heycode_tools::ToolEffect::ReadOnly
        }
        async fn run(
            &self,
            _: serde_json::Value,
            _: &heycode_tools::ToolCtx,
        ) -> Result<serde_json::Value, heycode_tools::ToolError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::json!("loaded"))
        }
    }

    #[tokio::test]
    async fn direct_registry_dispatch_cannot_bypass_skill_argument_policy() {
        use heycode_tools::Tool;
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool = ScopedChildTool {
            inner: std::sync::Arc::new(CountSkill(count.clone())),
            policy: std::sync::Arc::new(SubagentConfig {
                skills: Some(vec!["allowed".into()]),
                ..Default::default()
            }),
        };
        assert_eq!(tool.effect(), heycode_tools::ToolEffect::ReadOnly);
        assert_eq!(
            tool.spec().parameters["properties"]["name"]["enum"],
            serde_json::json!(["allowed"])
        );
        assert!(
            tool.run(
                serde_json::json!({"name":"foreign"}),
                &heycode_tools::ToolCtx::default()
            )
            .await
            .is_err()
        );
        assert!(
            tool.run_output(
                serde_json::json!({"name":"foreign"}),
                &heycode_tools::ToolCtx::default()
            )
            .await
            .is_err()
        );
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            tool.run_output(
                serde_json::json!({"name":"allowed"}),
                &heycode_tools::ToolCtx::default()
            )
            .await
            .is_ok()
        );
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn read_only_findings_require_explicit_local_reporting_authority() {
        let mut config = SubagentConfig {
            permissions: ChildPermissions::ReadOnly,
            ..Default::default()
        };
        assert!(!config.allows_tool("report_findings"));
        config.tools = Some(vec![
            "report_findings".to_owned(),
            "write".to_owned(),
            "bash".to_owned(),
        ]);
        assert!(config.allows_tool("report_findings"));
        assert!(!config.allows_tool("write"));
        assert!(!config.allows_tool("bash"));
        config.denied_tools.push("report_findings".to_owned());
        assert!(!config.allows_tool("report_findings"));
        config.denied_tools.clear();
        config.permissions = ChildPermissions::Deny;
        assert!(!config.allows_tool("report_findings"));
    }

    #[test]
    fn strict_policy_validation_and_deny_precedence() {
        assert!(serde_json::from_str::<SubagentConfig>(r#"{"unknown":true}"#).is_err());
        assert!(serde_json::from_str::<SubagentConfig>(r#"{"permissions":"bypass"}"#).is_err());
        let config = SubagentConfig {
            tools: Some(vec!["read".into(), "write".into()]),
            denied_tools: vec!["write".into()],
            ..Default::default()
        };
        config.validate().unwrap();
        assert!(config.allows_tool("read"));
        assert!(!config.allows_tool("write"));
        assert!(!config.allows_tool("bash"));
        assert!(
            SubagentConfig {
                max_turns: Some(0),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            SubagentConfig {
                inference_provider: Some("provider".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn readonly_agents_can_communicate_without_gaining_control_or_writes() {
        let mut config = SubagentConfig {
            permissions: ChildPermissions::ReadOnly,
            ..Default::default()
        };
        for name in [
            "send_message",
            "ask_user_question",
            "ask_user_question_async",
        ] {
            assert!(config.allows_tool(name));
        }
        for name in ["agent_control", "task", "write", "bash"] {
            assert!(!config.allows_tool(name));
        }
        config.denied_tools.push("send_message".into());
        assert!(!config.allows_tool("send_message"));
        config.tools = Some(vec!["read".into()]);
        assert!(!config.allows_tool("ask_user_question"));
    }

    #[test]
    fn scoped_skills_mcp_and_indirect_execution_are_enforced() {
        let config = SubagentConfig {
            skills: Some(vec!["allowed".into()]),
            mcp_servers: Some(vec!["server".into()]),
            ..Default::default()
        };
        assert!(config.allows_call("load_skill", &serde_json::json!({"name":"allowed"})));
        assert!(!config.allows_call("load_skill", &serde_json::json!({"name":"foreign"})));
        assert!(!config.allows_call("load_skill", &serde_json::json!({})));
        assert!(config.allows_tool("mcp__server__lookup"));
        assert!(!config.allows_tool("mcp__server-extra__lookup"));
        for tool in [
            "task",
            "workflow",
            "team",
            "background_shell",
            "schedule_create",
        ] {
            assert!(!config.allows_tool(tool));
        }
        assert!(
            !SubagentConfig {
                permissions: ChildPermissions::ReadOnly,
                ..Default::default()
            }
            .allows_tool("new-unclassified-tool")
        );
    }
}
