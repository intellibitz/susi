//! Per-tenant resource and data isolation (VC-201-078).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

static GLOBAL_ISOLATION: OnceLock<Mutex<TenantIsolation>> = OnceLock::new();

/// Access the global production [`TenantIsolation`] instance.
pub fn global() -> &'static Mutex<TenantIsolation> {
    GLOBAL_ISOLATION.get_or_init(|| Mutex::new(TenantIsolation::new()))
}

/// Run a closure with mutable access to the global [`TenantIsolation`] instance.
pub fn with_global<R, F: FnOnce(&mut TenantIsolation) -> R>(f: F) -> R {
    let mut guard = global().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Run a closure with read access to the global [`TenantIsolation`] instance.
pub fn with_global_ref<R, F: FnOnce(&TenantIsolation) -> R>(f: F) -> R {
    let guard = global().lock().unwrap_or_else(|e| e.into_inner());
    f(&guard)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantQuota {
    pub max_queue: usize,
    pub budget_usd_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantContext {
    pub tenant_id: String,
    pub workspace_root: String,
    pub memory_ns: String,
    pub tool_grants: BTreeSet<String>,
    pub quota: TenantQuota,
}

#[derive(Debug, Default)]
pub struct TenantIsolation {
    tenants: BTreeMap<String, TenantContext>,
    tokens: BTreeMap<String, String>,
    queue_lens: BTreeMap<String, usize>,
    spent_cents: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessVerdict {
    Allowed,
    CrossTenantDenied,
    QuotaExhausted,
    ToolDenied,
}

/// Resolve `/`-separated `.`/`..` segments against a stack, with no
/// filesystem access — these are virtual workspace roots, not real paths,
/// so lexical resolution (not `canonicalize`) is what a boundary check
/// must use.
fn normalize_virtual_path(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    format!("/{}", stack.join("/"))
}

/// `path` is inside `root` only when it equals `root` or nests directly
/// under it — a raw `starts_with` also admits any sibling that merely
/// shares the string prefix (`/t/alpha-evil` under `/t/alpha`), which this
/// rejects by requiring a boundary (`/`) after the matched root.
fn is_within_workspace(path: &str, root: &str) -> bool {
    let path = normalize_virtual_path(path);
    let root = normalize_virtual_path(root);
    path == root || path.starts_with(&format!("{root}/"))
}

impl TenantIsolation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new tenant with a default auth token. Refuses to silently
    /// rebind an existing tenant_id to a different root/grants/quota — that
    /// would let any caller swap another tenant's identity with no ownership
    /// check. Call `unregister` first for a deliberate replacement.
    pub fn register(&mut self, ctx: TenantContext) -> Result<(), String> {
        let default_token = format!("token_{}", ctx.tenant_id);
        self.register_with_token(ctx, &default_token)
    }

    /// Register a new tenant with an explicit authorization secret.
    pub fn register_with_token(
        &mut self,
        ctx: TenantContext,
        auth_token: &str,
    ) -> Result<(), String> {
        if self.tenants.contains_key(&ctx.tenant_id) {
            return Err(format!(
                "tenant {} is already registered; unregister it first to rebind",
                ctx.tenant_id
            ));
        }
        self.tokens
            .insert(ctx.tenant_id.clone(), auth_token.to_string());
        self.tenants.insert(ctx.tenant_id.clone(), ctx);
        Ok(())
    }

    /// Authenticate that `token` matches the registered token for `tenant_id`.
    pub fn authenticate(&self, tenant_id: &str, token: &str) -> bool {
        let Some(expected) = self.tokens.get(tenant_id) else {
            return false;
        };
        !token.is_empty() && expected == token
    }

    /// Deliberately remove a tenant, clearing its queue, spend, and token.
    pub fn unregister(&mut self, tenant_id: &str) {
        self.tenants.remove(tenant_id);
        self.tokens.remove(tenant_id);
        self.queue_lens.remove(tenant_id);
        self.spent_cents.remove(tenant_id);
    }

    /// Authorize and remove a tenant using its auth_token.
    pub fn unregister_authorized(&mut self, tenant_id: &str, token: &str) -> Result<(), String> {
        if !self.authenticate(tenant_id, token) {
            return Err(format!(
                "unauthorized: invalid auth_token for tenant {tenant_id}"
            ));
        }
        self.unregister(tenant_id);
        Ok(())
    }

    /// Find the tenant owning the workspace containing `path`, if any.
    pub fn find_tenant_for_workspace(&self, path: &str) -> Option<&TenantContext> {
        self.tenants
            .values()
            .find(|ctx| is_within_workspace(path, &ctx.workspace_root))
    }

    /// Resolve a path against the tenant workspace; foreign roots denied.
    pub fn read_path(&self, tenant_id: &str, path: &str) -> AccessVerdict {
        let Some(ctx) = self.tenants.get(tenant_id) else {
            return AccessVerdict::CrossTenantDenied;
        };
        if is_within_workspace(path, &ctx.workspace_root) {
            AccessVerdict::Allowed
        } else {
            AccessVerdict::CrossTenantDenied
        }
    }

    pub fn read_memory(&self, tenant_id: &str, ns: &str) -> AccessVerdict {
        let Some(ctx) = self.tenants.get(tenant_id) else {
            return AccessVerdict::CrossTenantDenied;
        };
        if ns == ctx.memory_ns {
            AccessVerdict::Allowed
        } else {
            AccessVerdict::CrossTenantDenied
        }
    }

    pub fn use_tool(&self, tenant_id: &str, tool: &str) -> AccessVerdict {
        let Some(ctx) = self.tenants.get(tenant_id) else {
            return AccessVerdict::CrossTenantDenied;
        };
        if ctx.tool_grants.contains(tool) {
            AccessVerdict::Allowed
        } else {
            AccessVerdict::ToolDenied
        }
    }

    pub fn enqueue(&mut self, tenant_id: &str) -> AccessVerdict {
        let Some(ctx) = self.tenants.get(tenant_id) else {
            return AccessVerdict::CrossTenantDenied;
        };
        let len = self.queue_lens.entry(tenant_id.to_string()).or_insert(0);
        if *len >= ctx.quota.max_queue {
            return AccessVerdict::QuotaExhausted;
        }
        *len = len.saturating_add(1);
        AccessVerdict::Allowed
    }

    /// Release one queue slot on completion — the live counterpart to
    /// `enqueue`. Without this, `max_queue` only ever counted up and a
    /// tenant that filled its queue once stayed wedged forever.
    pub fn dequeue(&mut self, tenant_id: &str) -> AccessVerdict {
        let Some(_ctx) = self.tenants.get(tenant_id) else {
            return AccessVerdict::CrossTenantDenied;
        };
        if let Some(len) = self.queue_lens.get_mut(tenant_id) {
            *len = len.saturating_sub(1);
        }
        AccessVerdict::Allowed
    }

    pub fn charge(&mut self, tenant_id: &str, cents: u64) -> AccessVerdict {
        let Some(ctx) = self.tenants.get(tenant_id) else {
            return AccessVerdict::CrossTenantDenied;
        };
        let spent = self.spent_cents.entry(tenant_id.to_string()).or_insert(0);
        if spent.saturating_add(cents) > ctx.quota.budget_usd_cents {
            return AccessVerdict::QuotaExhausted;
        }
        *spent = spent.saturating_add(cents);
        AccessVerdict::Allowed
    }

    /// Check whether a tenant is registered.
    pub fn is_registered(&self, tenant_id: &str) -> bool {
        self.tenants.contains_key(tenant_id)
    }

    /// Retrieve a tenant's context, if registered.
    pub fn get_context(&self, tenant_id: &str) -> Option<&TenantContext> {
        self.tenants.get(tenant_id)
    }

    /// Current live queue length for a tenant.
    pub fn queue_len(&self, tenant_id: &str) -> usize {
        self.queue_lens.get(tenant_id).copied().unwrap_or(0)
    }

    /// Current accumulated spend in USD cents for a tenant.
    pub fn spent_cents(&self, tenant_id: &str) -> u64 {
        self.spent_cents.get(tenant_id).copied().unwrap_or(0)
    }
}
