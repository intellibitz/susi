// Flags/redacts secret-token and exfiltration patterns loaded from config.
// Mandate 10: No Secret Leaks - Zero tolerance for tokens, credentials, or keys.

use crate::error::{EaiError, EaiResult};
use crate::sandbox::manager::SusiConfig;
use std::path::{Path, PathBuf};

pub struct SecurityDetector;

impl SecurityDetector {
    pub fn audit_action(_tool_name: &str, arg: &str, _workspace: &Path) -> EaiResult<()> {
        let _home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let global_dir = crate::sandbox::xdg::SusiDirs::config_dir();
        let cfg = SusiConfig::load(&global_dir).unwrap_or_default();
        let patterns = cfg.governance();

        let lower_arg = arg.to_lowercase();

        // 1. Secret Leak Check (Dynamic)
        for pattern in &patterns.secret_tokens {
            if arg.contains(pattern) {
                return Err(EaiError::governance(format!(
                    "Suspicious secret or API key pattern detected ('{}')",
                    pattern
                )));
            }
        }

        // 2. Exfiltration Check (Dynamic)
        for pattern in &patterns.exfiltration_vectors {
            if lower_arg.contains(&pattern.to_lowercase()) {
                return Err(EaiError::governance(format!(
                    "Suspicious network exfiltration pattern detected ('{}')",
                    pattern
                )));
            }
        }

        Ok(())
    }

    /// Deterministically redacts any configured secret-token pattern (and its
    /// trailing token-shaped characters) found in `text`. Unlike `audit_action`
    /// (which rejects an action outright), this transforms text so it is safe to
    /// persist to telemetry/audit logs — no reliance on an LLM's output happening
    /// to mention a sentinel word.
    pub fn redact(text: &str) -> String {
        let _home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let global_dir = crate::sandbox::xdg::SusiDirs::config_dir();
        let patterns = match SusiConfig::load(&global_dir) {
            Ok(cfg) => cfg.governance().secret_tokens,
            Err(_) => return text.to_string(),
        };

        let mut redacted = text.to_string();
        for pattern in &patterns {
            if pattern.is_empty() {
                continue;
            }
            if let Ok(re) = regex::Regex::new(&format!("{}[A-Za-z0-9_-]*", regex::escape(pattern)))
            {
                redacted = re.replace_all(&redacted, "[REDACTED]").to_string();
            }
        }
        redacted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_security_audit_safe_arg() {
        let ws = Path::new(".");
        assert!(SecurityDetector::audit_action("status", "cargo build", ws).is_ok());
    }

    #[test]
    fn test_security_audit_secret_leak() {
        let ws = Path::new(".");
        assert!(SecurityDetector::audit_action("reason", "sk-proj12345", ws).is_err());
    }

    #[test]
    fn test_redact_masks_token_not_just_prefix() {
        let redacted = SecurityDetector::redact("token=sk-proj12345abcXYZ rest of log");
        assert!(!redacted.contains("proj12345"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(redacted.contains("rest of log"));
    }
}
