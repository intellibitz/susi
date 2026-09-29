//! Scan outbound prompts/context for credentials before egress (VC-201-073).
//!
//! Corpus-tested detector over governance `secret_tokens` prefixes and
//! held `*_API_KEY`/`*_TOKEN`/`*_SECRET` env values. Policy chooses block
//! (refuse the send) or redact (ship a scrubbed copy).

use crate::susi_error::redact::{contains_secret_pattern, mask_credentials_from, redact_patterns};
use serde::{Deserialize, Serialize};

/// What to do when a secret is found in outbound text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptScanPolicy {
    /// Refuse egress; caller must not send the original text.
    Block,
    /// Replace matches with `[REDACTED]` and allow the scrubbed text.
    #[default]
    Redact,
}

/// One finding from the detector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSecretHit {
    /// Pattern or env-var name that matched.
    pub kind: String,
    /// Byte offset of the match start in the original text.
    pub offset: usize,
}

/// Result of scanning one outbound prompt/context blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptScanResult {
    pub hits: Vec<PromptSecretHit>,
    /// Text safe to send when policy is Redact (or when clean).
    pub redacted: String,
    pub policy: PromptScanPolicy,
}

impl PromptScanResult {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.hits.is_empty()
    }

    /// `true` when the caller must not egress the original text.
    #[must_use]
    pub fn blocked(&self) -> bool {
        !self.hits.is_empty() && matches!(self.policy, PromptScanPolicy::Block)
    }

    /// Text to send: `None` when blocked; otherwise the (possibly redacted) body.
    #[must_use]
    pub fn egress_text(&self) -> Option<&str> {
        if self.blocked() {
            None
        } else {
            Some(self.redacted.as_str())
        }
    }
}

/// Scan `text` with configured token prefixes and currently held env secrets.
#[must_use]
pub fn scan_prompt_secrets(
    text: &str,
    patterns: &[String],
    env_vars: impl IntoIterator<Item = (String, String)>,
    policy: PromptScanPolicy,
) -> PromptScanResult {
    let env_vars: Vec<(String, String)> = env_vars.into_iter().collect();
    let mut hits = Vec::new();

    for pattern in patterns.iter().filter(|p| !p.is_empty()) {
        for offset in crate::susi_error::redact::secret_match_starts(text, pattern) {
            hits.push(PromptSecretHit {
                kind: format!("pattern:{pattern}"),
                offset,
            });
        }
    }
    for (key, value) in &env_vars {
        if value.len() >= 8
            && (key.ends_with("_API_KEY") || key.ends_with("_TOKEN") || key.ends_with("_SECRET"))
        {
            let mut search_from = 0;
            while let Some(rel) = text[search_from..].find(value.as_str()) {
                let offset = search_from + rel;
                hits.push(PromptSecretHit {
                    kind: format!("env:{key}"),
                    offset,
                });
                search_from = offset + value.len();
            }
        }
    }
    hits.sort_by_key(|h| h.offset);
    hits.dedup_by(|a, b| a.offset == b.offset && a.kind == b.kind);

    let mut redacted = redact_patterns(patterns, text);
    redacted = mask_credentials_from(&redacted, env_vars);

    // Keep contains_secret_pattern reachable for callers that only need a bool.
    let _ = contains_secret_pattern(patterns, text);

    PromptScanResult {
        hits,
        redacted,
        policy,
    }
}

/// Convenience: scan with process env and the given patterns.
#[must_use]
pub fn scan_prompt_secrets_env(
    text: &str,
    patterns: &[String],
    policy: PromptScanPolicy,
) -> PromptScanResult {
    scan_prompt_secrets(text, patterns, std::env::vars(), policy)
}
