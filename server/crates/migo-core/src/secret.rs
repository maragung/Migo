//! A string that refuses to be printed.
//!
//! Every credential in the configuration is a [`Secret`]. The type has no
//! `Display`, its `Debug` prints `Secret(***)`, and reading the value requires
//! calling [`Secret::expose`] — a name chosen so that `grep -rn "\.expose()"`
//! enumerates every place in the codebase where a secret becomes a plain
//! string. That list should be short and should be reviewed.
//!
//! This does not make secrets safe; it makes leaking one require typing
//! something incriminating. Most credential leaks are a `{:?}` on a config
//! struct in a startup log line.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize;

/// An opaque credential.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reveals the value. Audit every call site.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length in bytes, safe to log.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when the secret carries no value.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Length is disclosed because it helps diagnose "wrong key" without
        // disclosing the key. Everything else is withheld.
        write!(f, "Secret(*** {} bytes)", self.0.len())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Best effort: the allocation is overwritten before it returns to the
        // allocator. It may already have been copied by a reallocation, which is
        // why long-lived key material lives in migo-crypto's zeroizing types.
        self.0.zeroize();
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Secret)
    }
}

impl Serialize for Secret {
    /// Serializes as the redaction marker, never the value. Config dumps and
    /// admin endpoints therefore cannot leak a credential by accident.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_the_value() {
        let secret = Secret::new("hunter2-but-longer");
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("18 bytes"), "{rendered}");
    }

    #[test]
    fn serialization_redacts() {
        let json = serde_json::to_string(&Secret::new("s3cr3t")).expect("serializes");
        assert_eq!(json, "\"***\"");
    }

    #[test]
    fn expose_returns_the_value() {
        assert_eq!(Secret::new("abc").expose(), "abc");
    }

    #[test]
    fn deserializes_from_a_plain_string() {
        let secret: Secret = serde_json::from_str("\"abc\"").expect("deserializes");
        assert_eq!(secret.expose(), "abc");
    }
}
