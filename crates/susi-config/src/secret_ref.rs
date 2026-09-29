//! Scoped secret references resolved only at the authorized boundary (VC-201-066).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub scope: String,
    pub name: String,
}

impl SecretRef {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s.strip_prefix("secret://")?;
        let (scope, name) = rest.split_once('/')?;
        if scope.is_empty() || name.is_empty() {
            return None;
        }
        Some(Self {
            scope: scope.to_string(),
            name: name.to_string(),
        })
    }

    #[must_use]
    pub fn display(&self) -> String {
        format!("secret://{}/{}", self.scope, self.name)
    }
}

#[derive(Debug, Default)]
pub struct SecretVault {
    values: std::collections::BTreeMap<(String, String), String>,
}

impl SecretVault {
    pub fn put(&mut self, scope: &str, name: &str, value: &str) {
        self.values
            .insert((scope.to_string(), name.to_string()), value.to_string());
    }

    /// Resolve only when `caller_scope` matches the ref scope.
    pub fn resolve(&self, r: &SecretRef, caller_scope: &str) -> Result<String, String> {
        if r.scope != caller_scope {
            return Err("unauthorized scope".into());
        }
        self.values
            .get(&(r.scope.clone(), r.name.clone()))
            .cloned()
            .ok_or_else(|| "missing secret".into())
    }
}

/// Plan/export/tracing must never disclose resolved values.
#[must_use]
pub fn redact_for_export(text: &str) -> String {
    // Strip anything after `resolved=` on the same token/line.
    let mut out = String::new();
    for part in text.split_whitespace() {
        if let Some(prefix) = part.strip_prefix("resolved=") {
            let _ = prefix;
            out.push_str("resolved=<redacted>");
        } else if part.contains("resolved=") {
            out.push_str("resolved=<redacted>");
        } else {
            out.push_str(part);
        }
        out.push(' ');
    }
    out.trim_end().to_string()
}
