//! One typed contract for discoverable external capabilities.
//!
//! A capability is data, regardless of whether it is reached through A2A,
//! MCP, HTTP, or an adapter. The bus performs the common preflight checks and
//! turns a successful invocation into a cost-attributed receipt; surface
//! crates only provide the small handler closure at the boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// External surface that hosts a capability implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySurface {
    /// Agent-to-agent capability.
    A2a,
    /// Model Context Protocol capability.
    Mcp,
    /// HTTP or JSON-RPC capability.
    Http,
    /// In-process or provider adapter capability.
    Adapter,
}

/// Immutable catalog entry shared by discovery and dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    /// Stable data identifier used by requests.
    pub id: String,
    /// Surface responsible for the actual invocation.
    pub surface: CapabilitySurface,
    /// Human-readable operation name.
    pub operation: String,
    /// Permission required from the requesting subject.
    pub permission: String,
    /// Maximum execution time accepted by the capability.
    pub timeout_ms: u64,
    /// Declared maximum cost for one invocation, in micro-units.
    pub cost_micros: u64,
}

impl CapabilityDescriptor {
    /// Build a catalog entry from data supplied by a surface adapter.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // The constructor mirrors the six-field wire descriptor.
    pub fn new(
        id: impl Into<String>,
        surface: CapabilitySurface,
        operation: impl Into<String>,
        permission: impl Into<String>,
        timeout_ms: u64,
        cost_micros: u64,
    ) -> Self {
        Self {
            id: id.into(),
            surface,
            operation: operation.into(),
            permission: permission.into(),
            timeout_ms,
            cost_micros,
        }
    }
}

/// Request context common to every capability surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRequest {
    /// Catalog identifier to invoke.
    pub capability_id: String,
    /// Subject receiving cost and audit attribution.
    pub subject: String,
    /// Permissions held by the subject for this invocation.
    pub permissions: BTreeSet<String>,
    /// Caller timeout ceiling.
    pub timeout_ms: u64,
    /// Caller budget ceiling, in micro-units.
    pub budget_micros: u64,
}

impl CapabilityRequest {
    /// Build a request with the subject's explicit permissions and ceilings.
    #[must_use]
    pub fn new(
        capability_id: impl Into<String>,
        subject: impl Into<String>,
        permissions: impl IntoIterator<Item = String>,
        timeout_ms: u64,
        budget_micros: u64,
    ) -> Self {
        Self {
            capability_id: capability_id.into(),
            subject: subject.into(),
            permissions: permissions.into_iter().collect(),
            timeout_ms,
            budget_micros,
        }
    }
}

/// Measured result supplied by a surface adapter after execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityInvocation {
    /// Surface output, retained as a typed JSON value at the boundary.
    pub output: Value,
    /// Measured elapsed time in milliseconds.
    pub elapsed_ms: u64,
    /// Measured cost in micro-units.
    pub cost_micros: u64,
}

/// One successful, cost-attributed capability receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityReceipt {
    /// Catalog identifier that ran.
    pub capability_id: String,
    /// Surface that supplied the result.
    pub surface: CapabilitySurface,
    /// Subject charged and audited for the invocation.
    pub subject: String,
    /// Measured elapsed time in milliseconds.
    pub elapsed_ms: u64,
    /// Measured cost in micro-units.
    pub cost_micros: u64,
    /// Surface output.
    pub output: Value,
}

/// A typed refusal from discovery or dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    /// No catalog entry exists for the requested identifier.
    UnknownCapability(String),
    /// The subject lacks the descriptor's permission.
    PermissionDenied(String),
    /// The invocation exceeded the caller or descriptor timeout.
    TimeoutExceeded { elapsed_ms: u64, limit_ms: u64 },
    /// The invocation exceeded the caller or descriptor cost ceiling.
    BudgetExceeded { cost_micros: u64, limit_micros: u64 },
    /// A surface adapter rejected the invocation.
    HandlerFailed(String),
    /// A catalog entry is not dispatchable.
    InvalidDescriptor(String),
}

/// In-memory typed capability catalog and dispatch preflight.
#[derive(Debug, Clone, Default)]
pub struct CapabilityBus {
    descriptors: std::collections::BTreeMap<String, CapabilityDescriptor>,
}

impl CapabilityBus {
    /// Create an empty capability catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one data-defined capability, refusing malformed or duplicate ids.
    pub fn register(&mut self, descriptor: CapabilityDescriptor) -> Result<(), CapabilityError> {
        if descriptor.id.trim().is_empty() || descriptor.operation.trim().is_empty() {
            return Err(CapabilityError::InvalidDescriptor(
                "id and operation must be non-empty".into(),
            ));
        }
        if descriptor.permission.trim().is_empty() || descriptor.timeout_ms == 0 {
            return Err(CapabilityError::InvalidDescriptor(
                "permission and timeout must be non-empty".into(),
            ));
        }
        if self.descriptors.contains_key(&descriptor.id) {
            return Err(CapabilityError::InvalidDescriptor(
                "capability id already registered".into(),
            ));
        }
        self.descriptors.insert(descriptor.id.clone(), descriptor);
        Ok(())
    }

    /// Discover all entries, sorted by their stable ids.
    #[must_use]
    pub fn discover(&self) -> Vec<CapabilityDescriptor> {
        self.descriptors.values().cloned().collect()
    }

    /// Discover entries hosted on one surface.
    #[must_use]
    pub fn discover_surface(&self, surface: CapabilitySurface) -> Vec<CapabilityDescriptor> {
        self.descriptors
            .values()
            .filter(|descriptor| descriptor.surface == surface)
            .cloned()
            .collect()
    }

    /// Dispatch through the shared permission, timeout, and cost contract.
    pub fn dispatch<F>(
        &self,
        request: &CapabilityRequest,
        handler: F,
    ) -> Result<CapabilityReceipt, CapabilityError>
    where
        F: FnOnce(&CapabilityDescriptor) -> Result<CapabilityInvocation, String>,
    {
        let descriptor = self
            .descriptors
            .get(&request.capability_id)
            .ok_or_else(|| CapabilityError::UnknownCapability(request.capability_id.clone()))?;
        if !request.permissions.contains(&descriptor.permission) {
            return Err(CapabilityError::PermissionDenied(
                descriptor.permission.clone(),
            ));
        }
        let timeout_limit = request.timeout_ms.min(descriptor.timeout_ms);
        if timeout_limit == 0 {
            return Err(CapabilityError::TimeoutExceeded {
                elapsed_ms: 0,
                limit_ms: timeout_limit,
            });
        }
        let invocation = handler(descriptor).map_err(CapabilityError::HandlerFailed)?;
        if invocation.elapsed_ms > timeout_limit {
            return Err(CapabilityError::TimeoutExceeded {
                elapsed_ms: invocation.elapsed_ms,
                limit_ms: timeout_limit,
            });
        }
        let cost_limit = request.budget_micros.min(descriptor.cost_micros);
        if invocation.cost_micros > cost_limit {
            return Err(CapabilityError::BudgetExceeded {
                cost_micros: invocation.cost_micros,
                limit_micros: cost_limit,
            });
        }
        Ok(CapabilityReceipt {
            capability_id: descriptor.id.clone(),
            surface: descriptor.surface,
            subject: request.subject.clone(),
            elapsed_ms: invocation.elapsed_ms,
            cost_micros: invocation.cost_micros,
            output: invocation.output,
        })
    }
}
