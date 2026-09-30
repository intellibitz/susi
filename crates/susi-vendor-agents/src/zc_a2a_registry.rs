//! A2A peer registry populated without manual edits.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aRegistry {
    pub peers: BTreeMap<String, String>,
}

impl A2aRegistry {
    pub fn upsert(&mut self, id: &str, endpoint: &str) {
        self.peers.insert(id.to_string(), endpoint.to_string());
    }

    #[must_use]
    pub fn lookup(&self, id: &str) -> Option<&str> {
        self.peers.get(id).map(String::as_str)
    }
}

#[cfg(test)]
mod zc_a2a_registry_tests {
    use super::*;

    #[test]
    fn zc_a2a_registry_upsert_and_lookup() {
        let mut r = A2aRegistry::default();
        r.upsert("p1", "http://127.0.0.1:9092");
        assert_eq!(r.lookup("p1"), Some("http://127.0.0.1:9092"));
    }
}
