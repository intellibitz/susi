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

/// Optional context bindings for authorization checks.
#[derive(Debug, Clone, Default)]
pub struct AuthContext<'a> {
    pub workspace: Option<&'a str>,
    pub mission: Option<&'a str>,
    pub capabilities: Option<&'a [String]>,
}

/// Bindings for a newly issued key.
#[derive(Debug, Clone, Default)]
pub struct IssueBindings {
    pub expiry_unix: Option<u64>,
    pub workspace: Option<String>,
    pub capabilities: Vec<String>,
    pub audience: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayKeyRecord {
    pub id: String,
    pub secret_hash: String,
    pub quota: KeyQuota,
    pub created_unix: u64,
    pub expiry_unix: Option<u64>,
    pub revoked: bool,
    pub tokens_used: u64,
    pub spend_micros_used: u64,
    pub workspace: Option<String>,
    pub mission: Option<String>,
    pub capabilities: Vec<String>,
    pub audience: Option<String>,
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
        self.issue_with_bindings_struct(id, secret, quota, &IssueBindings::default())
    }

    pub fn issue_with_bindings_struct(
        &self,
        id: &str,
        secret: &str,
        quota: KeyQuota,
        bindings: &IssueBindings,
    ) -> GatewayKeyRecord {
        let rec = GatewayKeyRecord {
            id: id.to_string(),
            secret_hash: hash_secret(secret),
            quota,
            created_unix: now_unix(),
            expiry_unix: bindings.expiry_unix,
            revoked: false,
            tokens_used: 0,
            spend_micros_used: 0,
            workspace: bindings.workspace.clone(),
            mission: None,
            capabilities: bindings.capabilities.clone(),
            audience: bindings.audience.clone(),
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

    pub fn set_mission(&self, id: &str, mission: &str) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rec) = g.keys.get_mut(id) else {
            return false;
        };
        rec.mission = Some(mission.to_string());
        g.audit.push(GatewayKeyAudit {
            at_unix: now_unix(),
            key_id: id.to_string(),
            action: "bind_mission".into(),
            detail: mission.to_string(),
        });
        true
    }

    pub fn revoke_by_mission_cancellation(&self, mission: &str) -> Vec<String> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        let revoked_ids: Vec<String> = g
            .keys
            .iter_mut()
            .filter_map(|(id, rec)| {
                if !rec.revoked && rec.mission.as_deref() == Some(mission) {
                    rec.revoked = true;
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        for id in &revoked_ids {
            g.audit.push(GatewayKeyAudit {
                at_unix: now,
                key_id: id.clone(),
                action: "revoke_on_mission_cancel".into(),
                detail: mission.to_string(),
            });
        }
        revoked_ids
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
        self.authorize_and_charge_with_context(
            id,
            secret,
            tokens,
            spend_micros,
            &AuthContext::default(),
        )
    }

    /// Authenticate and charge with context bindings; returns false when
    /// revoked, expired, bindings mismatch, or over budget.
    #[allow(clippy::too_many_arguments, clippy::collapsible_if)]
    pub fn authorize_and_charge_with_context(
        &self,
        id: &str,
        secret: &str,
        tokens: u64,
        spend_micros: u64,
        ctx: &AuthContext,
    ) -> bool {
        let now = now_unix();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rec) = g.keys.get(id).cloned() else {
            return false;
        };
        if rec.revoked || rec.secret_hash != hash_secret(secret) {
            return false;
        }
        if let Some(expiry) = rec.expiry_unix {
            if now > expiry {
                g.audit.push(GatewayKeyAudit {
                    at_unix: now,
                    key_id: id.to_string(),
                    action: "deny_expired".into(),
                    detail: format!("expiry={expiry}"),
                });
                return false;
            }
        }
        if let Some(required_ws) = ctx.workspace {
            if let Some(ref bound_ws) = rec.workspace {
                if bound_ws != required_ws {
                    g.audit.push(GatewayKeyAudit {
                        at_unix: now,
                        key_id: id.to_string(),
                        action: "deny_workspace_mismatch".into(),
                        detail: format!("required={required_ws}"),
                    });
                    return false;
                }
            }
        }
        if let Some(required_mission) = ctx.mission {
            if let Some(ref bound_mission) = rec.mission {
                if bound_mission != required_mission {
                    g.audit.push(GatewayKeyAudit {
                        at_unix: now,
                        key_id: id.to_string(),
                        action: "deny_mission_mismatch".into(),
                        detail: format!("required={required_mission}"),
                    });
                    return false;
                }
            }
        }
        if let Some(required_caps) = ctx.capabilities {
            for cap in required_caps {
                if !rec.capabilities.contains(cap) {
                    g.audit.push(GatewayKeyAudit {
                        at_unix: now,
                        key_id: id.to_string(),
                        action: "deny_capability_mismatch".into(),
                        detail: format!("required={cap}"),
                    });
                    return false;
                }
            }
        }
        if rec.tokens_used.saturating_add(tokens) > rec.quota.token_budget {
            g.audit.push(GatewayKeyAudit {
                at_unix: now,
                key_id: id.to_string(),
                action: "deny_token_budget".into(),
                detail: format!("need {tokens}"),
            });
            return false;
        }
        if rec.spend_micros_used.saturating_add(spend_micros) > rec.quota.spend_budget_micros {
            g.audit.push(GatewayKeyAudit {
                at_unix: now,
                key_id: id.to_string(),
                action: "deny_spend_budget".into(),
                detail: format!("need {spend_micros}"),
            });
            return false;
        }
        let Some(rec_mut) = g.keys.get_mut(id) else {
            return false;
        };
        rec_mut.tokens_used = rec_mut.tokens_used.saturating_add(tokens);
        rec_mut.spend_micros_used = rec_mut.spend_micros_used.saturating_add(spend_micros);
        g.audit.push(GatewayKeyAudit {
            at_unix: now,
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

    #[test]
    fn vc_201_072_mastery_binding_and_expiry() {
        let store = GatewayKeyStore::new();
        let current_time = now_unix();
        let future_expiry = current_time + 3600;
        let past_expiry = current_time - 1;

        let rec = store.issue_with_bindings_struct(
            "scoped-key",
            "secret-scoped",
            KeyQuota::default(),
            &IssueBindings {
                expiry_unix: Some(future_expiry),
                workspace: Some("workspace-1".to_string()),
                capabilities: vec!["read".to_string(), "write".to_string()],
                audience: Some("public".to_string()),
            },
        );

        assert_eq!(rec.workspace, Some("workspace-1".to_string()));
        assert_eq!(rec.mission, None);
        assert_eq!(
            rec.capabilities,
            vec!["read".to_string(), "write".to_string()]
        );
        assert_eq!(rec.audience, Some("public".to_string()));
        assert_eq!(rec.expiry_unix, Some(future_expiry));

        assert!(store.set_mission("scoped-key", "inference-mission"));
        let updated = store.get("scoped-key").unwrap();
        assert_eq!(updated.mission, Some("inference-mission".to_string()));

        let ctx1 = AuthContext {
            workspace: Some("workspace-1"),
            mission: Some("inference-mission"),
            capabilities: Some(&["read".to_string()]),
        };
        assert!(store.authorize_and_charge_with_context(
            "scoped-key",
            "secret-scoped",
            100,
            1000,
            &ctx1,
        ));

        let ctx2 = AuthContext {
            workspace: Some("workspace-2"),
            mission: Some("inference-mission"),
            capabilities: Some(&["read".to_string()]),
        };
        assert!(!store.authorize_and_charge_with_context(
            "scoped-key",
            "secret-scoped",
            100,
            1000,
            &ctx2,
        ));

        let ctx3 = AuthContext {
            workspace: Some("workspace-1"),
            mission: Some("other-mission"),
            capabilities: Some(&["read".to_string()]),
        };
        assert!(!store.authorize_and_charge_with_context(
            "scoped-key",
            "secret-scoped",
            100,
            1000,
            &ctx3,
        ));

        let ctx4 = AuthContext {
            workspace: Some("workspace-1"),
            mission: Some("inference-mission"),
            capabilities: Some(&["admin".to_string()]),
        };
        assert!(!store.authorize_and_charge_with_context(
            "scoped-key",
            "secret-scoped",
            100,
            1000,
            &ctx4,
        ));

        let expired_key = store.issue_with_bindings_struct(
            "expired-key",
            "secret-expired",
            KeyQuota::default(),
            &IssueBindings {
                expiry_unix: Some(past_expiry),
                workspace: Some("workspace-1".to_string()),
                capabilities: vec!["read".to_string()],
                audience: None,
            },
        );
        assert_eq!(expired_key.expiry_unix, Some(past_expiry));

        let ctx5 = AuthContext {
            workspace: Some("workspace-1"),
            mission: None,
            capabilities: None,
        };
        assert!(!store.authorize_and_charge_with_context(
            "expired-key",
            "secret-expired",
            1,
            1,
            &ctx5,
        ));

        let log = store.audit_log();
        assert!(log.iter().any(|a| a.action == "bind_mission"));
        assert!(log.iter().any(|a| a.action == "deny_workspace_mismatch"));
        assert!(log.iter().any(|a| a.action == "deny_mission_mismatch"));
        assert!(log.iter().any(|a| a.action == "deny_capability_mismatch"));
        assert!(log.iter().any(|a| a.action == "deny_expired"));
    }

    #[test]
    fn vc_201_072_mastery_cancellation_revokes_credentials() {
        let store = GatewayKeyStore::new();

        let rec1 = store.issue_with_bindings_struct(
            "key1",
            "secret1",
            KeyQuota::default(),
            &IssueBindings {
                expiry_unix: None,
                workspace: Some("workspace-a".to_string()),
                capabilities: vec!["read".to_string()],
                audience: None,
            },
        );
        assert!(!rec1.revoked);

        store.set_mission("key1", "mission-x");
        let _rec2 = store.issue_with_bindings_struct(
            "key2",
            "secret2",
            KeyQuota::default(),
            &IssueBindings {
                expiry_unix: None,
                workspace: Some("workspace-a".to_string()),
                capabilities: vec!["write".to_string()],
                audience: None,
            },
        );
        store.set_mission("key2", "mission-x");

        let _rec3 = store.issue_with_bindings_struct(
            "key3",
            "secret3",
            KeyQuota::default(),
            &IssueBindings {
                expiry_unix: None,
                workspace: Some("workspace-b".to_string()),
                capabilities: vec!["read".to_string()],
                audience: None,
            },
        );
        store.set_mission("key3", "mission-y");

        let ctx = AuthContext {
            workspace: None,
            mission: Some("mission-x"),
            capabilities: None,
        };
        assert!(store.authorize_and_charge_with_context("key1", "secret1", 1, 1, &ctx,));

        let revoked = store.revoke_by_mission_cancellation("mission-x");
        assert_eq!(revoked.len(), 2);
        assert!(revoked.contains(&"key1".to_string()));
        assert!(revoked.contains(&"key2".to_string()));

        assert!(!store.authorize_and_charge("key1", "secret1", 1, 1));
        assert!(!store.authorize_and_charge("key2", "secret2", 1, 1));

        assert!(store.authorize_and_charge("key3", "secret3", 1, 1));

        let log = store.audit_log();
        assert!(
            log.iter()
                .any(|a| a.action == "revoke_on_mission_cancel" && a.detail == "mission-x")
        );
        assert_eq!(
            log.iter()
                .filter(|a| a.action == "revoke_on_mission_cancel")
                .count(),
            2
        );
    }
}
