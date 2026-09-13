//! Explicit DeepSeek Harness SDK runtime launch and route configuration.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt::{Debug, Formatter};
use std::path::PathBuf;

use heycode_runtime::RuntimeContractError;

const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;

/// One exact Harness SDK process template and process-wide model route.
#[derive(Clone)]
pub struct DeepSeekHarnessRuntimeConfig {
    program: OsString,
    args: Vec<OsString>,
    artifacts: Vec<PathBuf>,
    environment: Vec<(OsString, OsString)>,
    provider: String,
    model: String,
    max_tokens: Option<u64>,
}

impl DeepSeekHarnessRuntimeConfig {
    /// Construct the documented SDK-runtime defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            program: OsString::from("dsh-jsonrpc-agent"),
            args: Vec::new(),
            artifacts: Vec::new(),
            environment: Vec::new(),
            provider: "deepseek-official".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_tokens: None,
        }
    }

    /// Replace the executable name or path resolved by the subprocess Provider.
    ///
    /// # Errors
    /// Empty, NUL-bearing, or over-4096-byte values are rejected.
    pub fn with_program(
        mut self,
        program: impl Into<OsString>,
    ) -> Result<Self, RuntimeContractError> {
        let program = program.into();
        if program.is_empty() || contains_nul(&program) || os_len(&program) > 4096 {
            return Err(invalid(
                "DeepSeek Harness program",
                "1..=4096 NUL-free bytes",
            ));
        }
        self.program = program;
        Ok(self)
    }

    /// Replace the exact argv tail used for every SDK runtime process.
    ///
    /// # Errors
    /// More than 128 values, NULs, or over 1 MiB of argv are rejected.
    pub fn with_args(mut self, args: Vec<OsString>) -> Result<Self, RuntimeContractError> {
        let bytes = args.iter().map(|value| os_len(value)).sum::<usize>();
        if args.len() > 128 || args.iter().any(|value| contains_nul(value)) || bytes > 1024 * 1024 {
            return Err(invalid(
                "DeepSeek Harness arguments",
                "at most 128 NUL-free values and 1 MiB",
            ));
        }
        self.args = args;
        Ok(self)
    }

    /// Bind additional immutable launch artifacts imported by the SDK process.
    ///
    /// This is intended for reviewed script bundles and Cordis configuration
    /// files when the resolved `program` is an interpreter. The runtime hashes
    /// each path at activation and rechecks it before every session spawn.
    ///
    /// # Errors
    /// More than 32 paths, non-absolute paths, duplicates, NULs, or paths over
    /// 4096 bytes are rejected. File identity is checked during plugin apply.
    pub fn with_artifacts(mut self, artifacts: Vec<PathBuf>) -> Result<Self, RuntimeContractError> {
        let mut seen = BTreeSet::new();
        if artifacts.len() > 32
            || artifacts.iter().any(|path| {
                !path.is_absolute()
                    || contains_nul(path.as_os_str())
                    || os_len(path.as_os_str()) > 4096
                    || !seen.insert(path.clone())
            })
        {
            return Err(invalid(
                "DeepSeek Harness artifacts",
                "at most 32 unique absolute NUL-free paths up to 4096 bytes",
            ));
        }
        self.artifacts = artifacts;
        Ok(self)
    }

    /// Replace the complete child environment.
    ///
    /// No ambient variable is inherited by this boundary.
    ///
    /// # Errors
    /// More than 128 entries, duplicate/empty/`=`-bearing names, NULs, or a
    /// combined environment larger than 1 MiB are rejected.
    pub fn with_environment(
        mut self,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<Self, RuntimeContractError> {
        let mut names = BTreeSet::new();
        let invalid_shape = environment.len() > 128
            || environment.iter().any(|(name, value)| {
                name.is_empty()
                    || contains_nul(name)
                    || contains_nul(value)
                    || name.to_string_lossy().contains('=')
                    || !names.insert(name.clone())
            });
        let bytes = environment
            .iter()
            .map(|(name, value)| os_len(name).saturating_add(os_len(value)))
            .sum::<usize>();
        if invalid_shape || bytes > 1024 * 1024 {
            return Err(invalid(
                "DeepSeek Harness environment",
                "at most 128 unique explicit entries and 1 MiB",
            ));
        }
        self.environment = environment;
        Ok(self)
    }

    /// Replace the process-wide provider and default model route.
    ///
    /// # Errors
    /// Both ids must be trimmed, control-free values up to 256 bytes.
    pub fn with_route(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, RuntimeContractError> {
        let provider = provider.into();
        let model = model.into();
        validate_id(&provider, "DeepSeek Harness provider")?;
        validate_id(&model, "DeepSeek Harness model")?;
        self.provider = provider;
        self.model = model;
        Ok(self)
    }

    /// Set the optional per-request SDK output-token cap.
    ///
    /// # Errors
    /// A configured value must be a positive JSON safe integer.
    pub fn with_max_tokens(
        mut self,
        max_tokens: Option<u64>,
    ) -> Result<Self, RuntimeContractError> {
        if max_tokens.is_some_and(|value| value == 0 || value > MAX_SAFE_JSON_INTEGER) {
            return Err(invalid(
                "DeepSeek Harness max tokens",
                "a positive JSON safe integer",
            ));
        }
        self.max_tokens = max_tokens;
        Ok(self)
    }

    pub(crate) fn program(&self) -> &OsStr {
        &self.program
    }

    pub(crate) fn args(&self) -> &[OsString] {
        &self.args
    }

    pub(crate) fn artifacts(&self) -> &[PathBuf] {
        &self.artifacts
    }

    pub(crate) fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) const fn max_tokens(&self) -> Option<u64> {
        self.max_tokens
    }
}

impl Default for DeepSeekHarnessRuntimeConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for DeepSeekHarnessRuntimeConfig {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeepSeekHarnessRuntimeConfig")
            .field("program", &"<redacted>")
            .field("argument_count", &self.args.len())
            .field("artifact_count", &self.artifacts.len())
            .field("environment_count", &self.environment.len())
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

fn validate_id(value: &str, field: &'static str) -> Result<(), RuntimeContractError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 256
        || value.chars().any(char::is_control)
    {
        Err(invalid(field, "1..=256 trimmed control-free bytes"))
    } else {
        Ok(())
    }
}

fn contains_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}

fn os_len(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

fn invalid(field: &'static str, requirement: &'static str) -> RuntimeContractError {
    RuntimeContractError::InvalidField { field, requirement }
}
