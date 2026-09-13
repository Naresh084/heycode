//! Operation-time exact-argv execution.

use std::collections::BTreeMap;

use heycode_credentials::{
    CredentialProvider, CredentialProviderId, CredentialProviderState, CredentialQuery,
    CredentialSecret, CredentialSource,
};
use heycode_exec::{
    OutputOverflowPolicy, ProcessErrorCode, ProcessExit, ProcessSpec, SubprocessService,
};

use crate::{CommandCredentialError, CommandCredentialSpec};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Read-only command provider. Specs are immutable; every `resolve` executes
/// again so external rotation becomes visible on the next operation.
#[derive(Clone)]
pub struct CommandCredentialProvider {
    id: CredentialProviderId,
    specs: BTreeMap<heycode_credentials::CredentialReference, CommandCredentialSpec>,
    subprocess: SubprocessService,
}

impl std::fmt::Debug for CommandCredentialProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandCredentialProvider")
            .field("provider", &"command")
            .field("spec_count", &self.specs.len())
            .finish_non_exhaustive()
    }
}

impl CommandCredentialProvider {
    /// Validate an immutable reference→command map.
    ///
    /// # Errors
    /// Duplicate references or provider-id construction fail loud.
    pub fn new<I>(specs: I) -> Result<Self, CommandCredentialError>
    where
        I: IntoIterator<Item = CommandCredentialSpec>,
    {
        let mut by_reference = BTreeMap::new();
        for spec in specs {
            let reference = spec.reference.clone();
            if by_reference.insert(reference.clone(), spec).is_some() {
                return Err(CommandCredentialError::DuplicateReference {
                    reference: reference.as_str().to_owned(),
                });
            }
        }
        Ok(Self {
            id: CredentialProviderId::new("command")
                .map_err(|_| CommandCredentialError::InvalidProviderId)?,
            specs: by_reference,
            subprocess: SubprocessService::local(),
        })
    }

    pub(crate) fn with_subprocess(mut self, subprocess: SubprocessService) -> Self {
        self.subprocess = subprocess;
        self
    }

    pub(crate) fn has_specs(&self) -> bool {
        !self.specs.is_empty()
    }

    fn execute(&self, spec: &CommandCredentialSpec) -> Result<CredentialSecret, String> {
        let spec = spec.clone();
        let subprocess = self.subprocess.clone();
        let worker = std::thread::Builder::new()
            .name("heycode-credential-command".to_owned())
            .spawn(move || execute_on_private_runtime(spec, subprocess))
            .map_err(|_| CommandCredentialError::Worker.to_string())?;
        worker
            .join()
            .map_err(|_| CommandCredentialError::Worker.to_string())?
            .map(CredentialSecret::new)
            .map_err(|error| error.to_string())
    }
}

impl CredentialProvider for CommandCredentialProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        5
    }

    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(if self.specs.contains_key(&query.reference) {
            CredentialProviderState::configured(CredentialSource::Command, false)
        } else {
            CredentialProviderState::unconfigured(false)
        })
    }

    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        self.specs
            .get(&query.reference)
            .map(|spec| self.execute(spec))
            .transpose()
    }
}

fn execute_on_private_runtime(
    spec: CommandCredentialSpec,
    subprocess: SubprocessService,
) -> Result<String, CommandCredentialError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| CommandCredentialError::Runtime)?;
    runtime.block_on(execute_async(spec, subprocess))
}

async fn execute_async(
    spec: CommandCredentialSpec,
    subprocess: SubprocessService,
) -> Result<String, CommandCredentialError> {
    let program = subprocess
        .resolve_program(std::ffi::OsStr::new(&spec.program))
        .map_err(|_| CommandCredentialError::Spawn)?;
    let mut environment = BTreeMap::new();
    for name in &spec.inherited_env {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(std::ffi::OsString::from(name), value);
        }
    }
    let cwd = std::env::current_dir().map_err(|_| CommandCredentialError::Spawn)?;
    let process = ProcessSpec::new(program, cwd)
        .and_then(|process| process.with_args(spec.args))
        .and_then(|process| process.with_environment(environment))
        .and_then(|process| process.with_timeout(Some(spec.timeout)))
        .and_then(|process| process.with_output_limit_bytes(MAX_OUTPUT_BYTES))
        .map(|process| process.with_output_overflow_policy(OutputOverflowPolicy::Error))
        .map_err(|_| CommandCredentialError::Spawn)?;
    let output = subprocess
        .output(process, tokio_util::sync::CancellationToken::new())
        .await
        .map_err(|error| match error.code() {
            ProcessErrorCode::OutputLimit => CommandCredentialError::OutputTooLarge,
            ProcessErrorCode::NotFound | ProcessErrorCode::Spawn => CommandCredentialError::Spawn,
            _ => CommandCredentialError::OutputRead,
        })?;
    match output.exit() {
        ProcessExit::Exited { code: 0 } => {}
        ProcessExit::TimedOut | ProcessExit::InactivityTimedOut => {
            return Err(CommandCredentialError::Timeout);
        }
        ProcessExit::Exited { .. } | ProcessExit::Signalled { .. } => {
            return Err(CommandCredentialError::UnsuccessfulExit);
        }
        _ => return Err(CommandCredentialError::UnsuccessfulExit),
    }
    let bytes = output.stdout().to_vec();
    normalize_output(bytes)
}

fn normalize_output(mut bytes: Vec<u8>) -> Result<String, CommandCredentialError> {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(CommandCredentialError::EmptyOutput);
    }
    if bytes.iter().any(|byte| matches!(*byte, 0 | b'\n' | b'\r')) {
        return Err(CommandCredentialError::InvalidOutput);
    }
    String::from_utf8(bytes).map_err(|_| CommandCredentialError::InvalidOutput)
}
