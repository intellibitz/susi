//! Gateway API keys with per-key quotas (VC-201-072 / T-CLAUDE-77).
//!
//! Revocable keys for the OpenAI-compatible gateway, each with rate and
//! spend budgets and an append-only audit trail.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Per-key quotas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyQuota {
    pub rate_limit_per_minute: u32,
    pub token_budget: u64,
    pub spend_budget_micros: u64,
}

impl Default for KeyQuota {
    fn default() -> Self {
        Self {
            rate_limit_per_minute: 60,
            token_budget: 1_000_000,
            spend_budget_micros: 10_000_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayKeyRecord {
    pub id: String,
    pub secret_hash: String,
    pub quota: KeyQuota,
    pub created_unix: u64,
    pub revoked: bool,
    pub tokens_used: u64,
    pub spend_micros_used: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayKeyAudit {
    pub at_unix: u64,
    pub key_id: String,
    pub action: String,
    pub detail: String,
}

/// In-memory gateway key store (durable backends can wrap the same API).
#[derive(Debug, Default)]
pub struct GatewayKeyStore {
    inner: Mutex<StoreInner>,
}

#[derive(Debug, Default)]
struct StoreInner {
    keys: HashMap<String, GatewayKeyRecord>,
    audit: Vec<GatewayKeyAudit>,
}

impl GatewayKeyStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issue(&self, id: &str, secret: &str, quota: KeyQuota) -> GatewayKeyRecord {
        let rec = GatewayKeyRecord {
            id: id.to_string(),
            secret_hash: hash_secret(secret),
            quota,
            created_unix: now_unix(),
            revoked: false,
            tokens_used: 0,
            spend_micros_used: 0,
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.keys.insert(id.to_string(), rec.clone());
        g.audit.push(GatewayKeyAudit {
            at_unix: now_unix(),
            key_id: id.to_string(),
            action: "issue".into(),
            detail: format!("rate={}", rec.quota.rate_limit_per_minute),
        });
        rec
    }

    pub fn revoke(&self, id: &str) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rec) = g.keys.get_mut(id) else {
            return false;
        };
        rec.revoked = true;
        g.audit.push(GatewayKeyAudit {
            at_unix: now_unix(),
            key_id: id.to_string(),
            action: "revoke".into(),
            detail: String::new(),
        });
        true
    }

    /// Authenticate and charge usage; returns false when revoked or over budget.
    pub fn authorize_and_charge(
        &self,
        id: &str,
        secret: &str,
        tokens: u64,
        spend_micros: u64,
    ) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rec) = g.keys.get_mut(id) else {
            return false;
        };
        if rec.revoked || rec.secret_hash != hash_secret(secret) {
            return false;
        }
        if rec.tokens_used.saturating_add(tokens) > rec.quota.token_budget {
            g.audit.push(GatewayKeyAudit {
                at_unix: now_unix(),
                key_id: id.to_string(),
                action: "deny_token_budget".into(),
                detail: format!("need {tokens}"),
            });
            return false;
        }
        if rec.spend_micros_used.saturating_add(spend_micros) > rec.quota.spend_budget_micros {
            g.audit.push(GatewayKeyAudit {
                at_unix: now_unix(),
                key_id: id.to_string(),
                action: "deny_spend_budget".into(),
                detail: format!("need {spend_micros}"),
            });
            return false;
        }
        rec.tokens_used = rec.tokens_used.saturating_add(tokens);
        rec.spend_micros_used = rec.spend_micros_used.saturating_add(spend_micros);
        g.audit.push(GatewayKeyAudit {
            at_unix: now_unix(),
            key_id: id.to_string(),
            action: "charge".into(),
            detail: format!("tokens={tokens} spend_micros={spend_micros}"),
        });
        true
    }

    pub fn audit_log(&self) -> Vec<GatewayKeyAudit> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .audit
            .clone()
    }

    pub fn get(&self, id: &str) -> Option<GatewayKeyRecord> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys
            .get(id)
            .cloned()
    }
}

#[cfg(test)]
mod gateway_keys_tests {
    use super::*;

    #[test]
    fn gateway_keys_issue_authorize_and_revoke() {
        let store = GatewayKeyStore::new();
        let rec = store.issue("k1", "secret-one", KeyQuota::default());
        assert!(!rec.revoked);
        assert!(store.authorize_and_charge("k1", "secret-one", 10, 100));
        assert!(!store.authorize_and_charge("k1", "wrong", 1, 1));
        assert!(store.revoke("k1"));
        assert!(!store.authorize_and_charge("k1", "secret-one", 1, 1));
        let log = store.audit_log();
        assert!(log.iter().any(|a| a.action == "issue"));
        assert!(log.iter().any(|a| a.action == "revoke"));
        assert!(log.iter().any(|a| a.action == "charge"));
    }

    #[test]
    fn gateway_keys_enforces_token_budget() {
        let store = GatewayKeyStore::new();
        store.issue(
            "k2",
            "s2",
            KeyQuota {
                rate_limit_per_minute: 10,
                token_budget: 5,
                spend_budget_micros: 1_000_000,
            },
        );
        assert!(store.authorize_and_charge("k2", "s2", 5, 0));
        assert!(!store.authorize_and_charge("k2", "s2", 1, 0));
        assert!(
            store
                .audit_log()
                .iter()
                .any(|a| a.action == "deny_token_budget")
        );
    }

    #[test]
    fn gateway_keys_enforces_spend_budget() {
        let store = GatewayKeyStore::new();
        store.issue(
            "k3",
            "s3",
            KeyQuota {
                rate_limit_per_minute: 10,
                token_budget: 1_000_000,
                spend_budget_micros: 50,
            },
        );
        assert!(!store.authorize_and_charge("k3", "s3", 0, 51));
        assert!(
            store
                .audit_log()
                .iter()
                .any(|a| a.action == "deny_spend_budget")
        );
    }
}
