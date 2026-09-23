//! Inter-application permission and message broker for agent-of-agents
//! orchestration — vendored, file-backed variant.
//!
//! Identical public API to `susi_core::broker`, but state lives under
//! `<cache>/bus/<pid>/broker/` (the same process-scoped rendezvous as
//! `IpcPlaneBus`) instead of in-memory maps, so independent vendored
//! `susi_core` copies and separate processes observe the same grants,
//! pending requests, and inboxes. Lifetime matches the old in-process
//! semantics: broker state is process-scoped, not durable across
//! restarts — reads scan every live pid dir under `bus/`, writes stay
//! in the owning process's dir so pid-dir sweeping preserves ownership.
//!
//! Layout: `grants/<key>.json`, `requests/<id>.json`,
//! `inbox/<recipient>/<ordered-name>.json`. Writes are atomic (tmp + rename).

use crate::susi_core::plane_bus_ipc::{enc, sibling_pid_dirs};
use crate::susi_paths::SusiDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("broker dir {}: {e}", parent.display()))?;
    }
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("broker write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("broker rename {}: {e}", path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn scan_dir<T: for<'de> Deserialize<'de>>(dir: &Path) -> Vec<(PathBuf, T)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name.ends_with(".tmp") {
            continue;
        }
        if let Some(v) = read_json::<T>(&e.path()) {
            out.push((e.path(), v));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// File-backed permission/message broker shared across copies and
/// processes through the `bus/` rendezvous.
pub struct IpcBroker {
    dir: PathBuf,
}

impl IpcBroker {
    /// Fresh isolated broker (fresh rendezvous dir — same semantics as the
    /// in-memory `new()`).
    pub fn new() -> Self {
        Self {
            dir: std::env::temp_dir().join(format!(
                "susi-broker-{}-{}",
                std::process::id(),
                now_nanos()
            )),
        }
    }

    fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Global broker — shared `<cache>/bus/<pid>/broker/` rendezvous so all
    /// copies in this process see the same state; reads scan sibling pid
    /// dirs so separate processes interop.
    pub fn global() -> &'static Self {
        static BROKER: OnceLock<IpcBroker> = OnceLock::new();
        BROKER.get_or_init(|| {
            Self::with_dir(
                SusiDirs::cache_dir()
                    .join("bus")
                    .join(std::process::id().to_string())
                    .join("broker"),
            )
        })
    }

    fn grants_dir(&self) -> PathBuf {
        self.dir.join("grants")
    }

    fn requests_dir(&self) -> PathBuf {
        self.dir.join("requests")
    }

    fn inbox_dir(&self, recipient: &str) -> PathBuf {
        self.dir.join("inbox").join(enc(recipient))
    }

    /// Own `broker/<sub>` dir first, then every sibling pid dir's — grants,
    /// requests, and inboxes cross process boundaries through the shared
    /// `bus/` root. `new()`'s temp dir isn't under a `bus/` root, so it
    /// stays hermetic.
    fn state_dirs(&self, sub: &str) -> Vec<PathBuf> {
        let Some(pid_dir) = self.dir.parent() else {
            return vec![self.dir.join(sub)];
        };
        let under_bus = pid_dir
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "bus");
        if !under_bus {
            return vec![self.dir.join(sub)];
        }
        sibling_pid_dirs(pid_dir)
            .into_iter()
            .map(|d| d.join("broker").join(sub))
            .collect()
    }

    /// Grant `grantee` permission to perform `scope.action` on `scope.resource`.
    pub fn grant(
        &self,
        grantor: &str,
        grantee: &str,
        scope: PermissionScope,
        ttl_secs: Option<u64>,
    ) -> PermissionGrant {
        let now = now_secs();
        let grant = PermissionGrant {
            grantee: grantee.into(),
            grantor: grantor.into(),
            scope: scope.clone(),
            granted_at: now,
            expires_at: ttl_secs.map(|ttl| now + ttl),
        };
        let _ = write_json(&self.grants_dir().join(enc(&scope.key(grantee))), &grant);
        grant
    }

    /// Record a permission grant into the context graph as an observation.
    #[allow(clippy::too_many_arguments)]
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
        crate::susi_core::context_graph::ContextGraph::global().record_external_context(
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
        let id = format!("req-{}-{}", std::process::id(), now_nanos());
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
            created_at: now_secs(),
            status: NegotiationStatus::Pending,
            resolved_at: None,
        };
        let _ = write_json(&self.requests_dir().join(enc(&id)), &req);
        req
    }

    /// Locate a request across all rendezvous dirs — returns the owning
    /// requests dir so `negotiate` writes the resolution back where the
    /// request lives.
    fn load_request(&self, request_id: &str) -> Option<(PathBuf, PermissionRequest)> {
        self.state_dirs("requests")
            .iter()
            .find_map(|d| read_json(&d.join(enc(request_id))).map(|r| (d.clone(), r)))
    }

    /// Grantor resolves a pending request. On approve, installs a grant.
    pub fn negotiate(
        &self,
        request_id: &str,
        actor: &str,
        approve: bool,
        workspace: Option<&Path>,
    ) -> Result<PermissionRequest, String> {
        let Some((req_dir, mut entry)) = self.load_request(request_id) else {
            return Err(format!("unknown permission request: {request_id}"));
        };
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
        let now = now_secs();
        if approve {
            let _grant = self.grant_and_record(
                actor,
                &entry.requester,
                entry.scope.clone(),
                entry.ttl_secs,
                workspace,
            );
            entry.status = NegotiationStatus::Granted;
            entry.resolved_at = Some(now);
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
            crate::susi_core::context_graph::ContextGraph::global().record_external_context(
                "ipc_broker",
                &format!("permission denied: {request_id}"),
                &payload,
                workspace,
                None,
            );
        }
        write_json(&req_dir.join(enc(request_id)), &entry)?;
        Ok(entry)
    }

    /// Pending requests addressed to `grantor` (or all when `grantor` is empty).
    pub fn pending_requests(&self, grantor: Option<&str>) -> Vec<PermissionRequest> {
        self.state_dirs("requests")
            .iter()
            .flat_map(|d| scan_dir::<PermissionRequest>(d))
            .map(|(_, r)| r)
            .filter(|r| r.status == NegotiationStatus::Pending)
            .filter(|r| grantor.is_none_or(|g| r.grantor == g))
            .collect()
    }

    /// Check whether `grantee` currently holds a valid grant for `scope`.
    pub fn is_permitted(&self, grantee: &str, scope: &PermissionScope) -> bool {
        let key = enc(&scope.key(grantee));
        self.state_dirs("grants")
            .iter()
            .find_map(|d| read_json::<PermissionGrant>(&d.join(&key)))
            .is_some_and(|g| g.expires_at.is_none_or(|exp| exp > now_secs()))
    }

    /// Revoke an existing grant — searched across all rendezvous dirs and
    /// removed wherever it was written.
    pub fn revoke(&self, grantee: &str, scope: &PermissionScope) -> Option<PermissionGrant> {
        let key = enc(&scope.key(grantee));
        for dir in self.state_dirs("grants") {
            let file = dir.join(&key);
            if let Some(grant) = read_json::<PermissionGrant>(&file) {
                let _ = std::fs::remove_file(&file);
                return Some(grant);
            }
        }
        None
    }

    /// List grants held by an identity.
    pub fn grants_for(&self, grantee: &str) -> Vec<PermissionGrant> {
        self.state_dirs("grants")
            .iter()
            .flat_map(|d| scan_dir::<PermissionGrant>(d))
            .map(|(_, g)| g)
            .filter(|g| g.grantee == grantee)
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
            created_at: now_secs(),
        };
        let name = format!("{:020}-{}", now_nanos(), std::process::id());
        write_json(&self.inbox_dir(to).join(name), &msg)?;
        Ok(msg)
    }

    /// Receive up to `limit` messages for `recipient` (oldest first; consumed
    /// messages are deleted). Scans every rendezvous dir's inbox — senders
    /// in other processes write under their own pid dir.
    pub fn receive(&self, recipient: &str, limit: usize) -> Vec<IpcMessage> {
        let mut out = Vec::new();
        for dir in self.state_dirs("inbox") {
            for (path, msg) in scan_dir::<IpcMessage>(&dir.join(enc(recipient))) {
                if out.len() >= limit {
                    return out;
                }
                out.push(msg);
                let _ = std::fs::remove_file(&path);
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

