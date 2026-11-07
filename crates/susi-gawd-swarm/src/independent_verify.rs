//! Independent evidence for swarm verification (VC-201-028).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReceipt {
    pub tool: String,
    pub digest: String,
}

impl ToolReceipt {
    /// Verifies structural and digest invariants of a [`ToolReceipt`].
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `tool` is not a valid identity or `digest` is not
    /// a canonical `sha256:<64-hex>` digest.
    pub fn verify_tool_receipt_invariants(&self) -> Result<(), String> {
        if !valid_identity(&self.tool) {
            return Err(format!(
                "ToolReceipt tool '{}' must be a non-empty whitespace-free identity",
                self.tool
            ));
        }
        if !is_digest(&self.digest) {
            return Err(format!(
                "ToolReceipt digest '{}' must be a canonical sha256:<64-hex> string",
                self.digest
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewConclusion {
    pub reviewer: String,
    pub pass: bool,
    pub receipts: Vec<ToolReceipt>,
    pub implementer: String,
}

impl ReviewConclusion {
    /// Verifies structural, reviewer-independence, and receipt-uniqueness invariants of a [`ReviewConclusion`].
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `reviewer` or `implementer` is invalid, `reviewer == implementer`,
    /// any receipt fails [`ToolReceipt::verify_tool_receipt_invariants`], receipt digests are duplicated, or
    /// `pass` is `true` while `receipts` is empty.
    pub fn verify_conclusion_invariants(&self) -> Result<(), String> {
        if !valid_identity(&self.reviewer) || !valid_identity(&self.implementer) {
            return Err(
                "ReviewConclusion reviewer and implementer must be valid non-empty identities"
                    .to_string(),
            );
        }
        if self.reviewer == self.implementer {
            return Err(format!(
                "ReviewConclusion reviewer '{}' must be independent from implementer",
                self.reviewer
            ));
        }
        if self.pass && self.receipts.is_empty() {
            return Err("ReviewConclusion pass=true requires at least one ToolReceipt".to_string());
        }
        let mut seen = BTreeSet::new();
        for r in &self.receipts {
            r.verify_tool_receipt_invariants()?;
            if !seen.insert(r.digest.as_str()) {
                return Err(format!(
                    "ReviewConclusion duplicate tool receipt digest '{}'",
                    r.digest
                ));
            }
        }
        Ok(())
    }
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

    /// Verifies structural, reviewer-independence, and cryptographic integrity invariants of a [`ReviewReceipt`].
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if any identity field is invalid, `reviewer == implementer`,
    /// any digest field is malformed, or `review_digest != expected_digest()`.
    pub fn verify_receipt_invariants(&self) -> Result<(), String> {
        if !valid_identity(&self.reviewer)
            || !valid_identity(&self.implementer)
            || !valid_identity(&self.task_id)
            || !valid_identity(&self.tool)
            || !valid_identity(&self.commit_sha)
        {
            return Err(
                "ReviewReceipt identity fields must be non-empty and whitespace-free".to_string(),
            );
        }
        if self.reviewer == self.implementer {
            return Err(format!(
                "ReviewReceipt reviewer '{}' must be distinct from implementer",
                self.reviewer
            ));
        }
        if !is_digest(&self.arguments_digest)
            || !is_digest(&self.result_digest)
            || !is_digest(&self.review_digest)
        {
            return Err(
                "ReviewReceipt arguments_digest, result_digest, and review_digest must be canonical sha256 digests"
                    .to_string(),
            );
        }
        if self.review_digest != self.expected_digest() {
            return Err("ReviewReceipt review_digest does not match expected_digest()".to_string());
        }
        Ok(())
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

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn digest_fields_is_length_framed_and_produces_canonical_sha256_digest() {
        let d1 = digest_fields(&["ab", "c"]);
        let d2 = digest_fields(&["a", "bc"]);
        assert!(is_digest(&d1));
        assert!(is_digest(&d2));
        assert_ne!(d1, d2);
        assert!(!is_digest("sha256:short"));
        assert!(!is_digest(
            "md5:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
    }

    #[test]
    fn acceptance_digests_are_canonical_and_deterministic() {
        let cmd_a = vec!["cargo".to_string(), "test".to_string()];
        let cmd_b = vec!["cargo".to_string(), "clippy".to_string()];
        let da = acceptance_arguments_digest(&cmd_a);
        let db = acceptance_arguments_digest(&cmd_b);
        let ds = acceptance_success_digest();
        assert!(is_digest(&da));
        assert!(is_digest(&db));
        assert!(is_digest(&ds));
        assert_ne!(da, db);
    }

    #[test]
    fn review_receipt_verifies_bound_fields_and_rejects_tampering() {
        let args_d = acceptance_arguments_digest(&["cargo".to_string(), "test".to_string()]);
        let res_d = acceptance_success_digest();
        let receipt = ReviewReceipt::new(
            "reviewer-1",
            "impl-1",
            "T-1",
            "cargo_test",
            args_d.clone(),
            res_d.clone(),
            "abc1234",
        );
        assert!(receipt.verifies(
            "reviewer-1",
            "impl-1",
            "T-1",
            "cargo_test",
            &args_d,
            &res_d,
            "abc1234"
        ));
        assert!(!receipt.verifies(
            "reviewer-2",
            "impl-1",
            "T-1",
            "cargo_test",
            &args_d,
            &res_d,
            "abc1234"
        ));
        assert!(!receipt.verifies(
            "reviewer-1",
            "impl-1",
            "T-1",
            "cargo_test",
            &args_d,
            &res_d,
            "tampered"
        ));
    }

    #[test]
    fn verification_satisfied_enforces_independent_reviewer_and_unique_canonical_receipts() {
        let d1 = digest_fields(&["run-1"]);
        let d2 = digest_fields(&["run-2"]);
        let valid = ReviewConclusion {
            reviewer: "verifier-agent".into(),
            pass: true,
            receipts: vec![
                ToolReceipt {
                    tool: "cargo_test".into(),
                    digest: d1.clone(),
                },
                ToolReceipt {
                    tool: "clippy".into(),
                    digest: d2,
                },
            ],
            implementer: "impl-agent".into(),
        };
        assert!(verification_satisfied(&valid, "impl-agent"));
        assert!(!verification_satisfied(&valid, "other-agent"));

        let mut self_review = valid.clone();
        self_review.reviewer = "impl-agent".into();
        assert!(!verification_satisfied(&self_review, "impl-agent"));

        let mut dup_digest = valid;
        dup_digest.receipts[1].digest = d1;
        assert!(!verification_satisfied(&dup_digest, "impl-agent"));
    }

    #[test]
    fn verification_satisfied_rejects_failed_pass_empty_receipts_and_invalid_tool_or_digest() {
        let d1 = digest_fields(&["ok-1"]);
        let base = ReviewConclusion {
            reviewer: "verifier-agent".into(),
            pass: false,
            receipts: vec![ToolReceipt {
                tool: "cargo_test".into(),
                digest: d1.clone(),
            }],
            implementer: "impl-agent".into(),
        };
        assert!(!verification_satisfied(&base, "impl-agent"));

        let mut empty_receipts = base.clone();
        empty_receipts.pass = true;
        empty_receipts.receipts.clear();
        assert!(!verification_satisfied(&empty_receipts, "impl-agent"));

        let mut bad_tool = base;
        bad_tool.pass = true;
        bad_tool.receipts[0].tool = "cargo test".into();
        assert!(!verification_satisfied(&bad_tool, "impl-agent"));
        assert!(bad_tool.verify_conclusion_invariants().is_err());

        let valid_rcpt = ToolReceipt {
            tool: "cargo_test".into(),
            digest: d1.clone(),
        };
        assert!(valid_rcpt.verify_tool_receipt_invariants().is_ok());

        let valid_conc = ReviewConclusion {
            reviewer: "verifier-agent".into(),
            pass: true,
            receipts: vec![valid_rcpt],
            implementer: "impl-agent".into(),
        };
        assert!(valid_conc.verify_conclusion_invariants().is_ok());

        let args_d = acceptance_arguments_digest(&["cargo".to_string(), "test".to_string()]);
        let res_d = acceptance_success_digest();
        let rr = ReviewReceipt::new(
            "reviewer-1",
            "impl-1",
            "T-1",
            "cargo_test",
            args_d,
            res_d,
            "abc1234",
        );
        assert!(rr.verify_receipt_invariants().is_ok());
        let mut tampered_rr = rr;
        tampered_rr.commit_sha = "def5678".into();
        assert!(tampered_rr.verify_receipt_invariants().is_err());
    }
}
