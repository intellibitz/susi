//! Multi-provider consensus for high-risk missions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsensusResult {
    pub agreed: bool,
    pub value: String,
}

/// Require a majority agreement across provider answers.
#[must_use]
pub fn consensus(answers: &[String]) -> ConsensusResult {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for a in answers {
        *counts.entry(a.as_str()).or_default() += 1;
    }
    let Some((value, n)) = counts.into_iter().max_by_key(|(_, n)| *n) else {
        return ConsensusResult {
            agreed: false,
            value: String::new(),
        };
    };
    ConsensusResult {
        agreed: n * 2 > answers.len(),
        value: value.into(),
    }
}

#[cfg(test)]
mod consensus_mode_tests {
    use super::*;

    #[test]
    fn consensus_mode_requires_majority() {
        let r = consensus(&["a".into(), "a".into(), "b".into()]);
        assert!(r.agreed);
        assert_eq!(r.value, "a");
        assert!(!consensus(&["a".into(), "b".into()]).agreed);
    }
}
