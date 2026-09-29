//! Independent evidence for swarm verification (VC-201-028).

use serde::{Deserialize, Serialize};
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

#[must_use]
pub fn verification_satisfied(c: &ReviewConclusion, implementer: &str) -> bool {
    if c.reviewer == implementer {
        return false; // implementer's own assertion is not independent
    }
    if !c.pass {
        return false;
    }
    if c.receipts.is_empty() {
        return false;
    }
    // Duplicate-only outputs (no unique receipt digests) fail.
    let digests: BTreeSet<_> = c.receipts.iter().map(|r| r.digest.as_str()).collect();
    !digests.is_empty() && digests.len() == c.receipts.len()
}
