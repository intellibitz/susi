//! Scan outbound prompts/context for credentials and enforce classification before egress (VC-201-073).
//!
//! Corpus-tested detector over governance `secret_tokens` prefixes,
//! held env credentials, obfuscated tokens, and classification labels.
//! Policy chooses block (refuse the send) or redact (ship a scrubbed copy).
//! Default policy is Block (fail-closed refusal).

use crate::susi_error::redact::{contains_secret_pattern, mask_credentials_from, redact_patterns};
use serde::{Deserialize, Serialize};

/// Data classification level for outbound prompts and artifacts.
///
/// Propagated through inference, MCP, A2A, generated tools, and artifacts.
/// Prevents private or local-only payloads from egressing to external / unapproved targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ClassificationLabel {
    /// Non-sensitive content permitted to egress to approved cloud targets.
    #[default]
    ApprovedCloud,
    /// Confidential to the local workspace; blocked from external egress.
    WorkspacePrivate,
    /// Strictly restricted to this local host; never leaves the machine.
    LocalOnly,
}

impl ClassificationLabel {
    #[must_use]
    pub fn is_local_only(&self) -> bool {
        matches!(self, Self::LocalOnly)
    }

    #[must_use]
    pub fn is_workspace_private(&self) -> bool {
        matches!(self, Self::WorkspacePrivate)
    }

    /// Whether this classification label permits external cloud egress.
    #[must_use]
    pub fn permits_cloud_egress(&self) -> bool {
        matches!(self, Self::ApprovedCloud)
    }

    /// Combine two classification labels, keeping the stricter classification.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        std::cmp::max(self, other)
    }
}

/// What to do when a secret or classification violation is found in outbound text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptScanPolicy {
    /// Refuse egress; caller must not send the original text (fail-closed default).
    #[default]
    Block,
    /// Replace matches with `[REDACTED]` and allow the scrubbed text when permitted.
    Redact,
}

/// One finding from the detector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSecretHit {
    /// Pattern, env-var name, or classification reason that matched.
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
    pub classification: ClassificationLabel,
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

/// Check if an environment variable key indicates a credential or secret.
fn is_credential_env_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    k.ends_with("_API_KEY")
        || k.ends_with("_TOKEN")
        || k.ends_with("_SECRET")
        || k.ends_with("_PASSWORD")
        || k.ends_with("_KEY")
        || k == "API_KEY"
        || k == "TOKEN"
        || k == "SECRET"
        || k == "PASSWORD"
        || k.contains("PASSWORD")
        || k.contains("SECRET")
        || k.contains("API_KEY")
}

/// Normalize text by stripping whitespace to detect character-spaced obfuscated secrets.
fn strip_whitespace(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Scan `text` with configured token prefixes, currently held env secrets, and default classification.
#[must_use]
pub fn scan_prompt_secrets(
    text: &str,
    patterns: &[String],
    env_vars: impl IntoIterator<Item = (String, String)>,
    policy: PromptScanPolicy,
) -> PromptScanResult {
    let classification = if text.contains("workspace-private") || text.contains("WORKSPACE_PRIVATE")
    {
        ClassificationLabel::WorkspacePrivate
    } else if text.contains("local-only") || text.contains("LOCAL_ONLY") {
        ClassificationLabel::LocalOnly
    } else {
        ClassificationLabel::ApprovedCloud
    };
    scan_prompt_classified(text, classification, patterns, env_vars, policy)
}

/// Scan `text` with explicit classification label, configured token prefixes, and env secrets.
#[must_use]
pub fn scan_prompt_classified(
    text: &str,
    classification: ClassificationLabel,
    patterns: &[String],
    env_vars: impl IntoIterator<Item = (String, String)>,
    policy: PromptScanPolicy,
) -> PromptScanResult {
    let env_vars: Vec<(String, String)> = env_vars.into_iter().collect();
    let mut hits = Vec::new();

    // 1. Classification check: non-ApprovedCloud labels cannot egress to cloud
    if !classification.permits_cloud_egress() {
        hits.push(PromptSecretHit {
            kind: format!("classification:{:?}", classification),
            offset: 0,
        });
    }

    // 2. Secret token prefixes
    for pattern in patterns.iter().filter(|p| !p.is_empty()) {
        for offset in crate::susi_error::redact::secret_match_starts(text, pattern) {
            hits.push(PromptSecretHit {
                kind: format!("pattern:{pattern}"),
                offset,
            });
        }
    }

    // 3. Env-held secrets (matching credential keys, min length 4, plus de-obfuscation)
    let stripped_text = strip_whitespace(text);

    for (key, value) in &env_vars {
        if value.len() >= 4 && is_credential_env_key(key) {
            // Verbatim check
            let mut search_from = 0;
            while let Some(rel) = text[search_from..].find(value.as_str()) {
                let offset = search_from + rel;
                hits.push(PromptSecretHit {
                    kind: format!("env:{key}"),
                    offset,
                });
                search_from = offset + value.len();
            }

            // Obfuscation check (e.g. character-spaced secret)
            let stripped_val = strip_whitespace(value);
            if stripped_val.len() >= 4 && stripped_text.contains(&stripped_val) {
                // Check if this wasn't already caught by verbatim
                if !text.contains(value.as_str()) {
                    hits.push(PromptSecretHit {
                        kind: format!("obfuscated_env:{key}"),
                        offset: 0,
                    });
                }
            }
        }
    }

    hits.sort_by_key(|h| h.offset);
    hits.dedup_by(|a, b| a.offset == b.offset && a.kind == b.kind);

    let mut redacted = redact_patterns(patterns, text);
    // Redact env credentials
    for (key, value) in &env_vars {
        if value.len() >= 4 && is_credential_env_key(key) {
            redacted = redacted.replace(value.as_str(), "[REDACTED]");
        }
    }
    redacted = mask_credentials_from(&redacted, env_vars);

    // Keep contains_secret_pattern reachable for callers that only need a bool.
    let _ = contains_secret_pattern(patterns, text);

    PromptScanResult {
        hits,
        redacted,
        policy,
        classification,
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
