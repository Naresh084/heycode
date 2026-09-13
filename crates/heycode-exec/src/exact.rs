//! Exact executable-image admission for capability-scoped process launches.

use std::fmt;
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use crate::{ProcessError, ProcessErrorCode};

const MAX_EXACT_EXECUTABLE_BYTES: usize = 64 * 1024 * 1024;

/// Immutable executable bytes bound to one caller-supplied SHA-256 identity.
///
/// The local Provider materializes these bytes in a private generation and
/// substitutes that path for the caller's ambient executable path immediately
/// before the common sandbox/process launch. `Debug` never exposes the bytes or
/// digest.
#[derive(Clone)]
pub struct ExactExecutable {
    bytes: Arc<[u8]>,
    #[cfg(unix)]
    digest: [u8; 32],
}

impl ExactExecutable {
    /// Admit exact bytes only when they match the algorithm-qualified digest.
    ///
    /// # Errors
    /// Empty/oversized bytes and non-canonical or mismatched SHA-256 identities
    /// fail with [`ProcessErrorCode::InvalidSpec`].
    pub fn new(bytes: impl Into<Vec<u8>>, expected_digest: &str) -> Result<Self, ProcessError> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > MAX_EXACT_EXECUTABLE_BYTES {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        let expected = parse_sha256(expected_digest)
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::InvalidSpec))?;
        let actual: [u8; 32] = Sha256::digest(&bytes).into();
        if actual != expected {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        Ok(Self {
            bytes: Arc::from(bytes),
            #[cfg(unix)]
            digest: actual,
        })
    }

    #[cfg(unix)]
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(unix)]
    pub(crate) const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

impl fmt::Debug for ExactExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactExecutable")
            .field("bytes", &self.bytes.len())
            .field("digest", &"[SHA256]")
            .finish()
    }
}

/// Maximum operating-system authority an exact child may receive.
///
/// A `true` field is an explicit ceiling, not proof the child needs or receives
/// that authority. The active sandbox may enforce a stricter result. A launch
/// fails before spawn whenever the current backend would expose an authority
/// whose field is `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessAuthority {
    filesystem_read: bool,
    filesystem_write: bool,
    network: bool,
    process_spawn: bool,
}

impl ProcessAuthority {
    /// Construct an explicit four-axis authority ceiling.
    #[must_use]
    pub const fn new(
        filesystem_read: bool,
        filesystem_write: bool,
        network: bool,
        process_spawn: bool,
    ) -> Self {
        Self {
            filesystem_read,
            filesystem_write,
            network,
            process_spawn,
        }
    }

    /// Construct an empty authority ceiling.
    #[must_use]
    pub const fn deny_all() -> Self {
        Self::new(false, false, false, false)
    }

    /// Whether host-policy-admitted filesystem reads are permitted.
    #[must_use]
    pub const fn filesystem_read(self) -> bool {
        self.filesystem_read
    }

    /// Whether host-policy-admitted filesystem writes are permitted.
    #[must_use]
    pub const fn filesystem_write(self) -> bool {
        self.filesystem_write
    }

    /// Whether host-policy-admitted networking is permitted.
    #[must_use]
    pub const fn network(self) -> bool {
        self.network
    }

    /// Whether the child may create descendants outside a host API.
    #[must_use]
    pub const fn process_spawn(self) -> bool {
        self.process_spawn
    }
}

fn parse_sha256(value: &str) -> Option<[u8; 32]> {
    let encoded = value.strip_prefix("sha256:")?;
    if encoded.len() != 64
        || encoded
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, chunk) in encoded.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        digest[index] = (high << 4) | low;
    }
    Some(digest)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
