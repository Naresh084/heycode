//! Typed keys for the type-erased service map.

/// Stable identifier for one context service or interception seam.
///
/// Owner crates publish `pub const` values of this type. Consumers import the
/// constant instead of repeating a string literal, so a renamed/mistyped key
/// becomes a compile error rather than a runtime `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceKey(&'static str);

impl ServiceKey {
    /// Define a stable key.
    ///
    /// # Panics
    /// Const-evaluation fails for an empty name or bytes outside lowercase
    /// ASCII letters, digits, `-`, `_`, and `/`.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        assert!(!name.is_empty(), "service key cannot be empty");
        let bytes = name.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            assert!(
                (byte >= b'a' && byte <= b'z')
                    || (byte >= b'0' && byte <= b'9')
                    || byte == b'-'
                    || byte == b'_'
                    || byte == b'/',
                "service key contains an invalid byte"
            );
            index += 1;
        }
        Self(name)
    }

    /// String representation used on config/protocol/diagnostic boundaries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ServiceKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
