//! Risk and secret patterns update themselves, signed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternBundle {
    pub version: u32,
    pub patterns: Vec<String>,
    pub signature_ok: bool,
}

/// Accept an update only when the signature verifies.
pub fn apply_signed_update(
    current: PatternBundle,
    incoming: PatternBundle,
) -> Result<PatternBundle, String> {
    if !incoming.signature_ok {
        return Err("refusing unsigned pattern update".into());
    }
    if incoming.version <= current.version {
        return Err("incoming version not newer".into());
    }
    Ok(incoming)
}

#[cfg(test)]
mod zc_risk_patterns_update_tests {
    use super::*;

    #[test]
    fn zc_risk_patterns_update_requires_signature() {
        let cur = PatternBundle {
            version: 1,
            patterns: vec!["AKIA".into()],
            signature_ok: true,
        };
        assert!(apply_signed_update(
            cur.clone(),
            PatternBundle {
                version: 2,
                patterns: vec!["AKIA".into(), "sk-".into()],
                signature_ok: false,
            }
        )
        .is_err());
        let next = apply_signed_update(
            cur,
            PatternBundle {
                version: 2,
                patterns: vec!["AKIA".into(), "sk-".into()],
                signature_ok: true,
            },
        )
        .unwrap();
        assert_eq!(next.version, 2);
        assert_eq!(next.patterns.len(), 2);
    }
}
