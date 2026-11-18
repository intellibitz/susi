//! Canonical board publication schema and validation used by native Git policy.
//! Board storage, transport and collaborative execution live in Commercial Apps.

use serde::{Deserialize, Serialize};

/// Where board items live on the storage remote and in every clone.
pub const ITEM_PREFIX: &str = "refs/board/items/";
/// Canonical board item schema version (parity with `susi-dev-workflow`).
pub const SCHEMA: &str = "susi-board-item-v1";
/// Allowed board item kinds (Mandate 76).
pub const KINDS: [&str; 12] = [
    "claim", "landed", "cut", "heads-up", "request", "answer", "handoff", "decision", "blocked",
    "stop", "lift", "note",
];
/// Maximum body payload size per item.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Maximum title length per item.
pub const MAX_TITLE_CHARS: usize = 200;
/// One stored item on the Susi Native Board (`susi-board-item-v1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardItem {
    pub schema: String,
    pub n: u64,
    pub kind: String,
    pub agent: String,
    pub host: String,
    pub posted_unix: u64,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub prev: String,
}

/// Input draft for posting a new board item.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardDraft {
    pub kind: String,
    pub agent: String,
    pub host: String,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// Validate the single-word identity used by board publication and runtime attribution.
pub fn validate_agent_word(agent: &str) -> Result<(), String> {
    if agent.is_empty() || agent.chars().any(char::is_whitespace) {
        return Err("agent identity must be a single non-empty word without whitespace".to_owned());
    }
    Ok(())
}

/// Refuse credential-bearing board titles and payloads before publication.
pub fn check_no_credentials(title: &str, body: &str) -> Result<(), String> {
    for text in [title, body] {
        if crate::susi_config::redact_credentials(text) != *text
            || (text.contains("-----BEGIN") && text.contains("PRIVATE KEY"))
        {
            return Err(
                "this text looks like it contains a credential; the board never carries secrets"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

/// Validate a draft before posting.
pub fn validate_draft(draft: &BoardDraft) -> Result<(), String> {
    if !KINDS.contains(&draft.kind.as_str()) {
        return Err(format!(
            "unknown kind `{}` (one of: {})",
            draft.kind,
            KINDS.join(", ")
        ));
    }
    validate_agent_word(draft.agent.trim())?;
    let title = draft.title.trim();
    if title.is_empty() || title.contains('\n') || title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "the title must be one non-empty line of at most {MAX_TITLE_CHARS} characters"
        ));
    }
    if draft.body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "the body is {} bytes; the limit is {MAX_BODY_BYTES}",
            draft.body.len()
        ));
    }
    if draft.kind == "request" && draft.to.as_deref().is_none_or(|to| to.trim().is_empty()) {
        return Err("a request names who is asked (--to <agent>)".to_owned());
    }
    if matches!(draft.kind.as_str(), "answer" | "lift") && draft.re.is_none() {
        return Err(format!(
            "`{}` names the item it refers to (--re <n>)",
            draft.kind
        ));
    }
    check_no_credentials(&draft.title, &draft.body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_rejected_from_board_posts() {
        let draft = BoardDraft {
            kind: "note".into(),
            agent: "antigravity".into(),
            host: "h1".into(),
            title: "oops".into(),
            body: "token sk-1234567890abcdef1234567890abcdef".into(),
            ..BoardDraft::default()
        };
        let err = validate_draft(&draft).expect_err("credential must be refused");
        assert!(err.contains("credential"), "{err}");
    }
}
