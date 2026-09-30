//! Harden the delegations ingress directory.

use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngressDecision {
    pub allow: bool,
    pub reason: String,
}

/// Reject path traversal and non-.json payloads in the ingress dir.
#[must_use]
pub fn validate_ingress_name(name: &str) -> IngressDecision {
    if name.is_empty() || name.contains('\0') {
        return IngressDecision {
            allow: false,
            reason: "empty or nul name".into(),
        };
    }
    let p = Path::new(name);
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return IngressDecision {
            allow: false,
            reason: "path traversal".into(),
        };
    }
    if p.components().count() != 1 {
        return IngressDecision {
            allow: false,
            reason: "must be a single file name".into(),
        };
    }
    if !name.ends_with(".json") {
        return IngressDecision {
            allow: false,
            reason: "only .json accepted".into(),
        };
    }
    IngressDecision {
        allow: true,
        reason: "ok".into(),
    }
}

#[cfg(test)]
mod delegation_ingress_tests {
    use super::*;

    #[test]
    fn delegation_ingress_rejects_traversal() {
        assert!(!validate_ingress_name("../x.json").allow);
        assert!(!validate_ingress_name("note.txt").allow);
        assert!(validate_ingress_name("job-1.json").allow);
    }
}
