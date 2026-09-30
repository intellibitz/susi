//! Per-tenant resource and data isolation (VC-201-078).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

impl TenantIsolation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, ctx: TenantContext) {
        self.tenants.insert(ctx.tenant_id.clone(), ctx);
    }

    /// Resolve a path against the tenant workspace; foreign roots denied.
    pub fn read_path(&self, tenant_id: &str, path: &str) -> AccessVerdict {
        let Some(ctx) = self.tenants.get(tenant_id) else {
            return AccessVerdict::CrossTenantDenied;
        };
        if path.starts_with(&ctx.workspace_root) {
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
}
