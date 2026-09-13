//! Opaque identifiers used across boundaries.

use std::fmt;

use uuid::Uuid;

macro_rules! opaque_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Mint a fresh random id.
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            /// Wrap an externally supplied id string (e.g. resumed sessions).
            #[must_use]
            pub fn from_raw(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            /// Borrow the underlying id text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

opaque_id!(
    SessionId,
    "Identifies one durable session (log directory + resume target)."
);
opaque_id!(
    CallId,
    "Identifies one model-initiated tool call within a turn."
);
opaque_id!(
    RequestId,
    "Identifies one resolved model request and its durable header/context/state."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_unique() {
        let a = SessionId::generate();
        let b = SessionId::generate();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 36);
    }

    #[test]
    fn raw_ids_round_trip() {
        let id = SessionId::from_raw("abc");
        assert_eq!(id.to_string(), "abc");
    }
}
