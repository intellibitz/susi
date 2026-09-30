//! Learned task classifier from mission traces.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnedClassifier {
    pub counts: BTreeMap<String, BTreeMap<String, u32>>,
}

impl LearnedClassifier {
    pub fn observe(&mut self, feature: &str, class: &str) {
        *self
            .counts
            .entry(feature.into())
            .or_default()
            .entry(class.into())
            .or_default() += 1;
    }

    #[must_use]
    pub fn predict(&self, feature: &str) -> Option<String> {
        self.counts
            .get(feature)?
            .iter()
            .max_by_key(|(_, n)| *n)
            .map(|(c, _)| c.clone())
    }
}

#[cfg(test)]
mod learned_classifier_tests {
    use super::*;

    #[test]
    fn learned_classifier_from_traces() {
        let mut c = LearnedClassifier::default();
        c.observe("fix bug", "coding");
        c.observe("fix bug", "coding");
        c.observe("fix bug", "research");
        assert_eq!(c.predict("fix bug").as_deref(), Some("coding"));
    }
}
