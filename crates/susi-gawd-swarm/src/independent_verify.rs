//! Independent evidence for swarm verification (VC-201-028).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReceipt {
    pub tool: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewConclusion {
    pub reviewer: String,
    pub pass: bool,
    pub receipts: Vec<ToolReceipt>,
    pub implementer: String,
}

/// A receipt emitted by an authenticated reviewer after it inspected one
/// exact acceptance result. The digest binds every field, so a copied
/// reviewer name or a made-up output digest cannot substitute for the
/// review that was actually performed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReceipt {
    /// Authenticated reviewer identity supplied by the verifier boundary.
    pub reviewer: String,
    /// Worker identity whose change was reviewed.
    pub implementer: String,
    /// Queue task whose declared acceptance was inspected.
    pub task_id: String,
    /// Acceptance tool/program that produced the result.
    pub tool: String,
    /// Digest of the exact acceptance argv.
    pub arguments_digest: String,
    /// Digest of the observed successful acceptance result.
    pub result_digest: String,
    /// Exact branch revision accepted and reviewed.
    pub commit_sha: String,
    /// Integrity digest over all fields above.
    pub review_digest: String,
}

impl ReviewReceipt {
    /// Create a receipt after the reviewer has observed the acceptance.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // flat fields mirror the signed receipt schema
    pub fn new(
        reviewer: impl Into<String>,
        implementer: impl Into<String>,
        task_id: impl Into<String>,
        tool: impl Into<String>,
        arguments_digest: impl Into<String>,
        result_digest: impl Into<String>,
        commit_sha: impl Into<String>,
    ) -> Self {
        let mut receipt = Self {
            reviewer: reviewer.into(),
            implementer: implementer.into(),
            task_id: task_id.into(),
            tool: tool.into(),
            arguments_digest: arguments_digest.into(),
            result_digest: result_digest.into(),
            commit_sha: commit_sha.into(),
            review_digest: String::new(),
        };
        receipt.review_digest = receipt.expected_digest();
        receipt
    }

    /// Verify the receipt against the authenticated reviewer and the exact
    /// acceptance/commit context observed by the closure gate.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // every bound field is independently checked
    pub fn verifies(
        &self,
        authenticated_reviewer: &str,
        expected_implementer: &str,
        expected_task: &str,
        expected_tool: &str,
        expected_arguments_digest: &str,
        expected_result_digest: &str,
        expected_commit: &str,
    ) -> bool {
        valid_identity(&self.reviewer)
            && valid_identity(&self.implementer)
            && valid_identity(&self.task_id)
            && valid_identity(&self.tool)
            && valid_identity(&self.commit_sha)
            && self.reviewer == authenticated_reviewer
            && self.implementer == expected_implementer
            && self.task_id == expected_task
            && self.tool == expected_tool
            && self.arguments_digest == expected_arguments_digest
            && self.result_digest == expected_result_digest
            && self.commit_sha == expected_commit
            && is_digest(&self.arguments_digest)
            && is_digest(&self.result_digest)
            && is_digest(&self.review_digest)
            && self.review_digest == self.expected_digest()
    }

    fn expected_digest(&self) -> String {
        digest_fields(&[
            &self.reviewer,
            &self.implementer,
            &self.task_id,
            &self.tool,
            &self.arguments_digest,
            &self.result_digest,
            &self.commit_sha,
        ])
    }
}

/// Digest one or more textual values with length framing. Length framing
/// avoids ambiguous concatenations such as ("ab", "c") vs ("a", "bc").
#[must_use]
pub fn digest_fields(fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    format!("sha256:{}", hex_encode(&hasher.finalize()))
}

/// Digest the exact argv passed to an acceptance runner.
#[must_use]
pub fn acceptance_arguments_digest(command: &[String]) -> String {
    let fields: Vec<&str> = command.iter().map(String::as_str).collect();
    digest_fields(&fields)
}

/// Stable digest for an observed successful acceptance result.
#[must_use]
pub fn acceptance_success_digest() -> String {
    digest_fields(&["exit_code", "0", "tests", "nonzero"])
}

/// Check the canonical digest representation used by review receipts.
#[must_use]
pub fn is_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_whitespace)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[must_use]
pub fn verification_satisfied(c: &ReviewConclusion, implementer: &str) -> bool {
    if !valid_identity(implementer)
        || !valid_identity(&c.reviewer)
        || !valid_identity(&c.implementer)
        || c.implementer != implementer
        || c.reviewer == implementer
    {
        return false; // implementer's own assertion is not independent
    }
    if !c.pass {
        return false;
    }
    if c.receipts.is_empty() {
        return false;
    }
    // Empty tools and arbitrary strings are not evidence. Receipt digests
    // must be canonical and unique; the bound receipt path additionally
    // checks their command/result/commit provenance.
    let digests: BTreeSet<_> = c
        .receipts
        .iter()
        .filter(|r| valid_identity(&r.tool) && is_digest(&r.digest))
        .map(|r| r.digest.as_str())
        .collect();
    digests.len() == c.receipts.len()
}
