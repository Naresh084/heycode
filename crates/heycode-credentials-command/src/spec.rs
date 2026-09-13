//! Validated exact-argv command specification.

use std::time::Duration;

use heycode_credentials::CredentialReference;

use crate::CommandCredentialError;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ARGS: usize = 128;
const MAX_COMPONENT_BYTES: usize = 16 * 1024;

/// One credential reference resolved by one exact executable/argv vector.
/// This type intentionally has no `Debug` implementation because argv may
/// contain sensitive operational metadata.
#[derive(Clone)]
pub struct CommandCredentialSpec {
    pub(crate) reference: CredentialReference,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) timeout: Duration,
    pub(crate) inherited_env: Vec<String>,
}

impl CommandCredentialSpec {
    /// Validate an exact executable and argv. No shell is added by the provider.
    ///
    /// # Errors
    /// Blank/NUL/oversized fields or excessive argv fail loud with safe text.
    pub fn new<I, S>(
        reference: CredentialReference,
        program: impl Into<String>,
        args: I,
    ) -> Result<Self, CommandCredentialError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let program = program.into();
        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        validate_component(
            &reference,
            &program,
            "program is blank, contains NUL or is too long",
        )?;
        if args.len() > MAX_ARGS {
            return Err(invalid(&reference, "argv has too many entries"));
        }
        for arg in &args {
            validate_component(&reference, arg, "argv contains NUL or an oversized entry")?;
        }
        Ok(Self {
            reference,
            program,
            args,
            timeout: DEFAULT_TIMEOUT,
            inherited_env: Vec::new(),
        })
    }

    /// Override the execution deadline.
    ///
    /// # Errors
    /// Zero or more than 60 seconds fails loud.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, CommandCredentialError> {
        if timeout.is_zero() || timeout > MAX_TIMEOUT {
            return Err(invalid(
                &self.reference,
                "timeout must be between 1 nanosecond and 60 seconds",
            ));
        }
        self.timeout = timeout;
        Ok(self)
    }

    /// Allowlist process environment names copied at execution time. All
    /// unlisted variables are removed from the child.
    ///
    /// # Errors
    /// Invalid/duplicate names fail loud without reading their values.
    pub fn with_inherited_env<I, S>(mut self, names: I) -> Result<Self, CommandCredentialError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut names: Vec<String> = names.into_iter().map(Into::into).collect();
        names.sort();
        if names.windows(2).any(|pair| pair[0] == pair[1])
            || names.iter().any(|name| !valid_env_name(name))
        {
            return Err(invalid(
                &self.reference,
                "inherited environment names are invalid or duplicated",
            ));
        }
        self.inherited_env = names;
        Ok(self)
    }
}

fn validate_component(
    reference: &CredentialReference,
    value: &str,
    reason: &'static str,
) -> Result<(), CommandCredentialError> {
    if value.is_empty() || value.len() > MAX_COMPONENT_BYTES || value.as_bytes().contains(&0) {
        Err(invalid(reference, reason))
    } else {
        Ok(())
    }
}

fn valid_env_name(name: &str) -> bool {
    name.len() <= 128
        && name
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && name
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn invalid(reference: &CredentialReference, reason: &'static str) -> CommandCredentialError {
    CommandCredentialError::InvalidSpec {
        reference: reference.as_str().to_owned(),
        reason,
    }
}
