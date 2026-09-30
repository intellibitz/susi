//! Agent-card cache, refresh and signature verification.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedCard {
    pub url: String,
    pub body: String,
    pub signature_ok: bool,
    pub fetched_unix: u64,
}

#[derive(Debug, Default, Clone)]
pub struct CardCache {
    cards: BTreeMap<String, CachedCard>,
}

impl CardCache {
    pub fn put(&mut self, card: CachedCard) -> Result<(), String> {
        if !card.signature_ok {
            return Err("refusing unsigned agent card".into());
        }
        self.cards.insert(card.url.clone(), card);
        Ok(())
    }

    #[must_use]
    pub fn get_fresh(&self, url: &str, now: u64, ttl_secs: u64) -> Option<&CachedCard> {
        self.cards
            .get(url)
            .filter(|c| now.saturating_sub(c.fetched_unix) <= ttl_secs)
    }
}

#[cfg(test)]
mod a2a_card_cache_tests {
    use super::*;

    #[test]
    fn a2a_card_cache_verifies_and_expires() {
        let mut c = CardCache::default();
        assert!(c
            .put(CachedCard {
                url: "http://x".into(),
                body: "{}".into(),
                signature_ok: false,
                fetched_unix: 1,
            })
            .is_err());
        c.put(CachedCard {
            url: "http://x".into(),
            body: "{}".into(),
            signature_ok: true,
            fetched_unix: 100,
        })
        .unwrap();
        assert!(c.get_fresh("http://x", 150, 100).is_some());
        assert!(c.get_fresh("http://x", 250, 100).is_none());
    }
}
