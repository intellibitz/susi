//! Structural secret redaction (VC-202-014).
//!
//! A [`Secret`] wraps secret material in a type that cannot print, serialize
//! or otherwise expose it by accident: `Debug`, `Display` and `Serialize`
//! all emit a fixed marker, and the value is reachable only through
//! [`Secret::expose`], which names the operation so every use is auditable.
//!
//! This is structural, not a filter bolted on at the end: a `#[derive(Debug)]`
//! struct that holds a `Secret` field cannot leak the secret through that
//! field, whatever other code does with the struct.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The marker that replaces a secret in every printable form.
pub const REDACTED: &str = "[REDACTED]";

/// Secret material that cannot be printed, serialized or logged.
///
/// `Clone`/`PartialEq`/`Eq`/`Hash` are deliberately allowed — copying or
/// comparing a secret does not reveal it. The only path to the value is
/// [`Secret::expose`], which callers audit because it is named.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret value — the only path that reveals it. Every caller of
    /// this method is a deliberate disclosure point.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The marker the printable forms emit.
    #[must_use]
    pub const fn redacted() -> &'static str {
        REDACTED
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(REDACTED)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A struct that derives `Debug` — if its `Secret` field leaked, this
    /// derived impl would leak it too. It must not.
    #[derive(Debug)]
    struct ProviderConfig {
        api_key: Secret,
        model: String,
    }

    /// Acceptance: no printable path reveals a secret, and the value is
    /// reachable only through the named `expose` disclosure point.
    #[test]
    fn secrets_never_logged() {
        let secret = Secret::new("sk-1234567890abcdef");

        // Debug never leaks.
        let debugged = format!("{secret:?}");
        assert!(!debugged.contains("1234567890"), "{debugged}");
        assert_eq!(debugged, REDACTED);

        // Display never leaks.
        assert_eq!(format!("{secret}"), REDACTED);

        // Serialize never leaks.
        let json = serde_json::to_string(&secret).expect("serialize");
        assert_eq!(json, "\"[REDACTED]\"");
        assert!(!json.contains("1234567890"), "{json}");

        // A derived Debug struct holding a Secret does not leak through it.
        let cfg = ProviderConfig {
            api_key: Secret::new("sk-topsecret"),
            model: "gpt-5".to_string(),
        };
        let cfg_dbg = format!("{cfg:?}");
        assert!(!cfg_dbg.contains("topsecret"), "{cfg_dbg}");
        assert!(cfg_dbg.contains("gpt-5"));
        // The fields are still reachable — the struct is usable, only the
        // secret's printable forms are masked.
        assert_eq!(cfg.model, "gpt-5");
        assert_eq!(cfg.api_key.expose(), "sk-topsecret");

        // The only disclosure point is named and explicit.
        assert_eq!(secret.expose(), "sk-1234567890abcdef");
    }
}
