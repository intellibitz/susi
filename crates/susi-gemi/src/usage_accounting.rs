//! Track real token usage per provider.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLedger {
    pub prompt_tokens: BTreeMap<String, u64>,
    pub completion_tokens: BTreeMap<String, u64>,
}

impl UsageLedger {
    pub fn record(&mut self, provider: &str, prompt: u64, completion: u64) {
        *self.prompt_tokens.entry(provider.into()).or_default() += prompt;
        *self.completion_tokens.entry(provider.into()).or_default() += completion;
    }

    #[must_use]
    pub fn total_tokens(&self, provider: &str) -> u64 {
        self.prompt_tokens.get(provider).copied().unwrap_or(0)
            + self.completion_tokens.get(provider).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod usage_accounting_tests {
    use super::*;

    #[test]
    fn usage_accounting_sums_per_provider() {
        let mut l = UsageLedger::default();
        l.record("openai", 10, 5);
        l.record("openai", 2, 3);
        assert_eq!(l.total_tokens("openai"), 20);
        assert_eq!(l.total_tokens("missing"), 0);
    }
}
