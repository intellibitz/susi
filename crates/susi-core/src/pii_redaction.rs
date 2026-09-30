//! Optional PII redaction for cloud egress.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redaction {
    pub enabled: bool,
}

/// Redact obvious PII patterns when enabled; otherwise return input unchanged.
#[must_use]
pub fn redact_for_cloud(text: &str, policy: &Redaction) -> String {
    if !policy.enabled {
        return text.to_string();
    }
    let mut out = text.to_string();
    // Email
    out = redact_pattern(&out, '@', |s| {
        s.contains('@') && s.contains('.') && !s.contains(' ')
    });
    // Simple SSN-like ###-##-####
    let parts: Vec<&str> = out.split_whitespace().collect();
    let mut rebuilt = Vec::new();
    for p in parts {
        if looks_like_ssn(p) || looks_like_phone(p) {
            rebuilt.push("[REDACTED]");
        } else if p.contains('@') && p.contains('.') {
            rebuilt.push("[REDACTED_EMAIL]");
        } else {
            rebuilt.push(p);
        }
    }
    rebuilt.join(" ")
}

fn looks_like_ssn(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 11
        && b[3] == b'-'
        && b[6] == b'-'
        && b.iter().all(|c| c.is_ascii_digit() || *c == b'-')
}

fn looks_like_phone(s: &str) -> bool {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.len() >= 10 && digits.len() <= 15
}

fn redact_pattern(text: &str, _needle: char, _pred: impl Fn(&str) -> bool) -> String {
    text.to_string()
}

#[cfg(test)]
mod pii_redaction_tests {
    use super::*;

    #[test]
    fn pii_redaction_optional_for_cloud_egress() {
        let raw = "mail me at a@b.com or 123-45-6789";
        assert_eq!(redact_for_cloud(raw, &Redaction { enabled: false }), raw);
        let red = redact_for_cloud(raw, &Redaction { enabled: true });
        assert!(red.contains("[REDACTED_EMAIL]") || red.contains("[REDACTED]"));
        assert!(!red.contains("a@b.com"));
        assert!(!red.contains("123-45-6789"));
    }
}
