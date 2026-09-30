//! Secret scanning / redaction on by default.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionPolicy {
    pub enabled: bool,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[must_use]
pub fn redact_secrets(policy: &RedactionPolicy, text: &str) -> String {
    if !policy.enabled {
        return text.to_string();
    }
    let mut out = text.to_string();
    for prefix in ["sk-", "sk-ant-", "gsk_", "hf_"] {
        while let Some(i) = out.find(prefix) {
            let end = out[i..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .map(|n| i + n)
                .unwrap_or(out.len());
            out.replace_range(i..end, "<redacted>");
        }
    }
    out
}

#[cfg(test)]
mod zc_redaction_default_tests {
    use super::*;

    #[test]
    fn zc_redaction_default_on() {
        let p = RedactionPolicy::default();
        assert!(p.enabled);
        let r = redact_secrets(&p, "token sk-abc123 end");
        assert!(r.contains("<redacted>"));
        assert!(!r.contains("sk-abc"));
    }
}
