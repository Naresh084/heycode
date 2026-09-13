//! Native inspection assistants available on every configured inference provider.
use crate::{
    ChildPermissions, SubagentConfig, SubagentContinuation, SubagentError, SubagentErrorCode,
    SubagentPreset, SubagentProviderId, SubagentSeed,
};

/// Built-in native presets. They inherit the current native inference provider,
/// model and effort unless a higher-precedence file preset overrides them.
///
/// # Errors
/// Invalid built-in metadata, treated as an implementation error.
pub fn builtin_native_presets() -> Result<Vec<SubagentPreset>, SubagentError> {
    let definitions = [
        (
            "reviewer",
            "Code reviewer",
            "Review the requested code or changes for concrete correctness defects and regressions. Inspect relevant source and tests with read-only tools. Explain each actionable finding with a file location, triggering conditions, impact and a focused fix. Use report_findings for actionable findings, citing exact read revisions and line ranges. Distinguish verified behavior from uncertainty; report no findings when evidence does not support a defect. Do not edit files or run commands.",
        ),
        (
            "advisor",
            "Technical advisor",
            "Investigate the user's technical question using available source and documentation. Give a concise recommendation supported by concrete code evidence, alternatives and tradeoffs. State assumptions and unresolved questions. Use read-only inspection; do not make changes, execute commands or delegate.",
        ),
        (
            "security-review",
            "Security reviewer",
            "Inspect the requested code for actionable security defects. Trace attacker-controlled input through authorization and validation to a reachable sensitive operation. Calibrate impact to the evidence and deployment assumptions. Report precise source locations, reproduction conditions and remediation; separate speculative hardening from validated findings. Use read-only tools. Do not execute payloads, mutate files, contact targets or delegate.",
        ),
    ];
    definitions
        .into_iter()
        .map(|(id, display, instructions)| {
            SubagentPreset::new(
                id,
                display,
                instructions,
                Some(SubagentProviderId::new("native").map_err(|error| {
                    SubagentError::new(SubagentErrorCode::Failed, error.to_string())
                })?),
                SubagentSeed::Fresh,
                SubagentContinuation::OneShot,
            )
            .map_err(|error| SubagentError::new(SubagentErrorCode::Failed, error.to_string()))?
            .with_config(SubagentConfig {
                permissions: ChildPermissions::ReadOnly,
                tools: Some(
                    [
                        "read",
                        "glob",
                        "grep",
                        "lsp_servers",
                        "lsp_definition",
                        "lsp_references",
                        "lsp_diagnostics",
                    ]
                    .into_iter()
                    .chain((id == "reviewer").then_some("report_findings"))
                    .map(str::to_owned)
                    .collect(),
                ),
                ..Default::default()
            })
        })
        .collect()
}
