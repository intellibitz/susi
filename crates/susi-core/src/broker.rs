//! Inter-application permission and message broker for agent-of-agents
//! orchestration.
//!
//! This is a substrate-level broker: applications, agents, or external adapters
//! register identities, request/grant/negotiate capability-scoped permissions,
//! and exchange typed messages. It is not a kernel-level IPC replacement.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// A capability-scoped permission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PermissionScope {
    pub resource: String,
    pub action: String,
}

impl PermissionScope {
    pub fn new(resource: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            resource: resource.into(),
            action: action.into(),
        }
    }

    pub fn key(&self, grantee: &str) -> String {
        format!("{}:{}:{}", grantee, self.resource, self.action)
    }
}

/// A recorded permission grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionGrant {
    pub grantee: String,
    pub grantor: String,
    pub scope: PermissionScope,
    pub granted_at: u64,
    pub expires_at: Option<u64>,
}

/// Outcome of a permission negotiation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegotiationStatus {
    Pending,
    Granted,
    Denied,
}

/// A pending or resolved permission request between identities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub requester: String,
    /// Identity asked to approve (defaults to `"susi"` when omitted at request time).
    pub grantor: String,
    pub scope: PermissionScope,
    pub ttl_secs: Option<u64>,
    pub created_at: u64,
    pub status: NegotiationStatus,
    pub resolved_at: Option<u64>,
}

/// One typed message between broker identities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage {
    pub from: String,
    pub to: String,
    pub topic: String,
    pub payload: serde_json::Value,
    pub created_at: u64,
}

/// In-memory permission/message broker.
pub struct IpcBroker {
    grants: DashMap<String, PermissionGrant>,
    requests: DashMap<String, PermissionRequest>,
    inboxes: DashMap<String, VecDeque<IpcMessage>>,
    next_request_id: AtomicU64,
}

impl IpcBroker {
    pub fn new() -> Self {
        Self {
            grants: DashMap::new(),
            requests: DashMap::new(),
            inboxes: DashMap::new(),
            next_request_id: AtomicU64::new(1),
        }
    }

    /// Global broker instance.
    pub fn global() -> &'static Self {
        static BROKER: OnceLock<IpcBroker> = OnceLock::new();
        BROKER.get_or_init(IpcBroker::new)
    }

    /// Current Unix timestamp in seconds.
    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Grant `grantee` permission to perform `scope.action` on `scope.resource`.
    pub fn grant(
        &self,
        grantor: &str,
        grantee: &str,
        scope: PermissionScope,
        ttl_secs: Option<u64>,
    ) -> PermissionGrant {
        let now = Self::now();
        let grant = PermissionGrant {
            grantee: grantee.into(),
            grantor: grantor.into(),
            scope: scope.clone(),
            granted_at: now,
            expires_at: ttl_secs.map(|ttl| now + ttl),
        };
        self.grants.insert(scope.key(grantee), grant.clone());
        grant
    }

    /// Record a permission grant into the context graph as an observation.
    pub fn grant_and_record(
        &self,
        grantor: &str,
        grantee: &str,
        scope: PermissionScope,
        ttl_secs: Option<u64>,
        workspace: Option<&Path>,
    ) -> PermissionGrant {
        let grant = self.grant(grantor, grantee, scope, ttl_secs);
        let payload = serde_json::json!({
            "grantor": grantor,
            "grantee": grantee,
            "resource": grant.scope.resource,
            "action": grant.scope.action,
            "expires_at": grant.expires_at,
        });
        let label = format!(
            "permission grant: {} -> {} on {}:{}",
            grantor, grantee, grant.scope.resource, grant.scope.action
        );
        crate::context_graph::ContextGraph::global().record_external_context(
            "ipc_broker",
            &label,
            &payload,
            workspace,
            None,
        );
        grant
    }

    /// Open a permission negotiation: `requester` asks `grantor` for `scope`.
    pub fn request(
        &self,
        requester: &str,
        grantor: &str,
        scope: PermissionScope,
        ttl_secs: Option<u64>,
    ) -> PermissionRequest {
        let id = format!(
            "req-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        );
        let req = PermissionRequest {
            id: id.clone(),
            requester: requester.into(),
            grantor: if grantor.is_empty() {
                "susi".into()
            } else {
                grantor.into()
            },
            scope,
            ttl_secs,
            created_at: Self::now(),
            status: NegotiationStatus::Pending,
            resolved_at: None,
        };
        self.requests.insert(id, req.clone());
        req
    }

    /// Grantor resolves a pending request. On approve, installs a grant.
    pub fn negotiate(
        &self,
        request_id: &str,
        actor: &str,
        approve: bool,
        workspace: Option<&Path>,
    ) -> Result<PermissionRequest, String> {
        let mut entry = self
            .requests
            .get_mut(request_id)
            .ok_or_else(|| format!("unknown permission request: {request_id}"))?;
        if entry.status != NegotiationStatus::Pending {
            return Err(format!(
                "request {request_id} already resolved as {:?}",
                entry.status
            ));
        }
        let admin = actor == "susi";
        if !admin && entry.grantor != actor {
            return Err(format!(
                "{actor} cannot negotiate request owned by {}",
                entry.grantor
            ));
        }
        let now = Self::now();
        if approve {
            let grant = self.grant_and_record(
                actor,
                &entry.requester,
                entry.scope.clone(),
                entry.ttl_secs,
                workspace,
            );
            entry.status = NegotiationStatus::Granted;
            entry.resolved_at = Some(now);
            let _ = grant;
        } else {
            entry.status = NegotiationStatus::Denied;
            entry.resolved_at = Some(now);
            let payload = serde_json::json!({
                "request_id": request_id,
                "requester": entry.requester,
                "grantor": actor,
                "resource": entry.scope.resource,
                "action": entry.scope.action,
                "status": "denied",
            });
            crate::context_graph::ContextGraph::global().record_external_context(
                "ipc_broker",
                &format!("permission denied: {request_id}"),
                &payload,
                workspace,
                None,
            );
        }
        Ok(entry.clone())
    }

    /// Pending requests addressed to `grantor` (or all when `grantor` is empty).
    pub fn pending_requests(&self, grantor: Option<&str>) -> Vec<PermissionRequest> {
        self.requests
            .iter()
            .filter(|e| e.value().status == NegotiationStatus::Pending)
            .filter(|e| grantor.is_none_or(|g| e.value().grantor == g))
            .map(|e| e.value().clone())
            .collect()
    }

    /// Check whether `grantee` currently holds a valid grant for `scope`.
    pub fn is_permitted(&self, grantee: &str, scope: &PermissionScope) -> bool {
        self.grants
            .get(&scope.key(grantee))
            .is_some_and(|g| g.expires_at.is_none_or(|exp| exp > Self::now()))
    }

    /// Revoke an existing grant.
    pub fn revoke(&self, grantee: &str, scope: &PermissionScope) -> Option<PermissionGrant> {
        self.grants.remove(&scope.key(grantee)).map(|(_, g)| g)
    }

    /// List grants held by an identity.
    pub fn grants_for(&self, grantee: &str) -> Vec<PermissionGrant> {
        self.grants
            .iter()
            .filter(|e| e.value().grantee == grantee)
            .map(|e| e.value().clone())
            .collect()
    }

    /// Send a message from `from` to `to`. Requires `from` to hold a dispatch
    /// grant for `to`, unless `from` is the grantor/admin identity `"susi"`.
    pub fn send(
        &self,
        from: &str,
        to: &str,
        topic: &str,
        payload: serde_json::Value,
    ) -> Result<IpcMessage, String> {
        let admin = from == "susi";
        let dispatch_scope = PermissionScope::new(format!("app://{to}"), "dispatch");
        if !admin && !self.is_permitted(from, &dispatch_scope) {
            return Err(format!(
                "{from} lacks dispatch permission for {to}; request it first"
            ));
        }
        let msg = IpcMessage {
            from: from.into(),
            to: to.into(),
            topic: topic.into(),
            payload,
            created_at: Self::now(),
        };
        self.inboxes
            .entry(to.into())
            .or_default()
            .value_mut()
            .push_back(msg.clone());
        Ok(msg)
    }

    /// Receive up to `limit` messages for `recipient`.
    pub fn receive(&self, recipient: &str, limit: usize) -> Vec<IpcMessage> {
        let mut out = Vec::new();
        if let Some(mut inbox) = self.inboxes.get_mut(recipient) {
            while out.len() < limit && !inbox.value().is_empty() {
                if let Some(msg) = inbox.value_mut().pop_front() {
                    out.push(msg);
                }
            }
        }
        out
    }
}

impl Default for IpcBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_grants_and_checks_permissions() {
        let broker = IpcBroker::new();
        let scope = PermissionScope::new("app://editor", "read");
        assert!(!broker.is_permitted("agent-a", &scope));
        broker.grant("susi", "agent-a", scope.clone(), None);
        assert!(broker.is_permitted("agent-a", &scope));
        broker.revoke("agent-a", &scope);
        assert!(!broker.is_permitted("agent-a", &scope));
    }

    #[test]
    fn broker_expires_grants() {
        let broker = IpcBroker::new();
        let scope = PermissionScope::new("app://editor", "write");
        broker.grant("susi", "agent-b", scope.clone(), Some(0));
        assert!(!broker.is_permitted("agent-b", &scope));
    }

    #[test]
    fn broker_requires_dispatch_to_send() {
        let broker = IpcBroker::new();
        let payload = serde_json::json!({"x": 1});
        assert!(broker
            .send("agent-a", "agent-b", "ping", payload.clone())
            .is_err());
        broker.grant(
            "susi",
            "agent-a",
            PermissionScope::new("app://agent-b", "dispatch"),
            None,
        );
        assert!(broker.send("agent-a", "agent-b", "ping", payload).is_ok());
        let msgs = broker.receive("agent-b", 10);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].topic, "ping");
    }

    #[test]
    fn broker_negotiates_permissions() {
        let broker = IpcBroker::new();
        let scope = PermissionScope::new("app://editor", "write");
        let req = broker.request("agent-a", "editor-owner", scope.clone(), Some(3600));
        assert_eq!(req.status, NegotiationStatus::Pending);
        assert_eq!(broker.pending_requests(Some("editor-owner")).len(), 1);
        assert!(broker.negotiate(&req.id, "stranger", true, None).is_err());
        let resolved = broker
            .negotiate(&req.id, "editor-owner", true, None)
            .unwrap();
        assert_eq!(resolved.status, NegotiationStatus::Granted);
        assert!(broker.is_permitted("agent-a", &scope));
        assert!(broker.pending_requests(Some("editor-owner")).is_empty());
    }

    #[test]
    fn broker_denies_negotiation() {
        let broker = IpcBroker::new();
        let scope = PermissionScope::new("app://vault", "read");
        let req = broker.request("agent-b", "susi", scope.clone(), None);
        let resolved = broker.negotiate(&req.id, "susi", false, None).unwrap();
        assert_eq!(resolved.status, NegotiationStatus::Denied);
        assert!(!broker.is_permitted("agent-b", &scope));
    }
}
