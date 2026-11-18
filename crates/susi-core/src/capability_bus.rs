//! One typed contract for discoverable external capabilities.
//!
//! A capability is data, regardless of whether it is reached through A2A,
//! MCP, HTTP, or an adapter. The bus performs the common preflight checks and
//! turns a successful invocation into a cost-attributed receipt; surface
//! crates only provide the small handler closure at the boundary.
//!
//! Every dispatch runs inside a [`DelegationScope`] (`crate::delegation`):
//! [`CapabilityBus::dispatch_in`] opens one child scope per invocation and
//! hands it to the handler, so a handler that dispatches again nests a driver
//! inside an engine under the same permission, budget, deadline, depth, cycle
//! and cancellation checks. [`CapabilityBus::dispatch`] starts from a root
//! scope built from the request itself.

use crate::delegation::{
    DelegationError, DelegationHop, DelegationLimits, DelegationRequest, DelegationScope,
    ROOT_CAPABILITY,
};
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
    /// Delegation hops from the root to this invocation, root first: who
    /// asked whom, so a nested result keeps its provenance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_path: Vec<DelegationHop>,
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
    /// The recursive delegation contract refused the invocation: cancelled,
    /// past its deadline, too deep, a cycle, permission amplification or an
    /// exhausted parent budget.
    Delegation(DelegationError),
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

    /// Dispatch through the shared permission, timeout, and cost contract,
    /// from a root delegation scope built from the request itself.
    pub fn dispatch<F>(
        &self,
        request: &CapabilityRequest,
        handler: F,
    ) -> Result<CapabilityReceipt, CapabilityError>
    where
        F: FnOnce(&CapabilityDescriptor) -> Result<CapabilityInvocation, String>,
    {
        let root = Self::root_scope(request)?;
        self.dispatch_in(&root, request, |descriptor, _scope| handler(descriptor))
    }

    /// The root scope a top-level request runs under: the subject, holding
    /// exactly the request's permissions, budget and timeout.
    pub fn root_scope(request: &CapabilityRequest) -> Result<DelegationScope, CapabilityError> {
        let hop = DelegationHop::new(request.subject.clone(), ROOT_CAPABILITY)
            .map_err(CapabilityError::Delegation)?;
        DelegationScope::root(
            hop,
            request.permissions.iter().cloned(),
            request.budget_micros,
            Some(request.timeout_ms),
            DelegationLimits::default(),
        )
        .map_err(CapabilityError::Delegation)
    }

    /// Dispatch as a child of `parent`: the request's permissions must be a
    /// subset of the parent's, its cost ceiling is reserved from the parent's
    /// budget, and the handler receives the child scope so it can dispatch
    /// further capabilities nested under it.
    pub fn dispatch_in<F>(
        &self,
        parent: &DelegationScope,
        request: &CapabilityRequest,
        handler: F,
    ) -> Result<CapabilityReceipt, CapabilityError>
    where
        F: FnOnce(&CapabilityDescriptor, &DelegationScope) -> Result<CapabilityInvocation, String>,
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
        let cost_limit = request.budget_micros.min(descriptor.cost_micros);
        let hop = DelegationHop::new(request.subject.clone(), descriptor.id.clone())
            .map_err(CapabilityError::Delegation)?;
        let scope = parent
            .delegate(DelegationRequest {
                hop,
                permissions: request.permissions.clone(),
                budget_micros: cost_limit,
                timeout_ms: timeout_limit,
            })
            .map_err(CapabilityError::Delegation)?;
        let invocation = handler(descriptor, &scope).map_err(CapabilityError::HandlerFailed)?;
        // A result produced after an ancestor cancelled the work is not
        // reported as success.
        scope
            .check_cancelled()
            .map_err(CapabilityError::Delegation)?;
        if invocation.elapsed_ms > timeout_limit {
            return Err(CapabilityError::TimeoutExceeded {
                elapsed_ms: invocation.elapsed_ms,
                limit_ms: timeout_limit,
            });
        }
        if invocation.cost_micros > cost_limit {
            return Err(CapabilityError::BudgetExceeded {
                cost_micros: invocation.cost_micros,
                limit_micros: cost_limit,
            });
        }
        // Nested dispatches already spent from this scope; the handler's own
        // cost must fit in what they left.
        scope
            .charge(invocation.cost_micros)
            .map_err(|_| CapabilityError::BudgetExceeded {
                cost_micros: invocation
                    .cost_micros
                    .saturating_add(cost_limit.saturating_sub(scope.remaining_micros())),
                limit_micros: cost_limit,
            })?;
        Ok(CapabilityReceipt {
            capability_id: descriptor.id.clone(),
            surface: descriptor.surface,
            subject: request.subject.clone(),
            elapsed_ms: invocation.elapsed_ms,
            cost_micros: invocation.cost_micros,
            output: invocation.output,
            delegation_path: scope.path(),
        })
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn register_rejects_empty_fields_and_duplicate_capability_ids() {
        let mut bus = CapabilityBus::new();
        assert!(matches!(
            bus.register(CapabilityDescriptor::new(
                "",
                CapabilitySurface::Mcp,
                "op",
                "perm",
                100,
                10
            )),
            Err(CapabilityError::InvalidDescriptor(_))
        ));
        assert!(matches!(
            bus.register(CapabilityDescriptor::new(
                "cap.1",
                CapabilitySurface::Mcp,
                "op",
                "perm",
                0,
                10
            )),
            Err(CapabilityError::InvalidDescriptor(_))
        ));
        let valid =
            CapabilityDescriptor::new("cap.1", CapabilitySurface::Mcp, "op", "perm", 100, 10);
        bus.register(valid.clone()).unwrap();
        assert!(matches!(
            bus.register(valid),
            Err(CapabilityError::InvalidDescriptor(_))
        ));
    }

    #[test]
    fn discover_and_discover_surface_filter_sorted_descriptors() {
        let mut bus = CapabilityBus::new();
        bus.register(CapabilityDescriptor::new(
            "b.http",
            CapabilitySurface::Http,
            "get",
            "net.read",
            500,
            20,
        ))
        .unwrap();
        bus.register(CapabilityDescriptor::new(
            "a.mcp",
            CapabilitySurface::Mcp,
            "list",
            "mcp.read",
            500,
            10,
        ))
        .unwrap();
        let all = bus.discover();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "a.mcp");
        assert_eq!(all[1].id, "b.http");
        let mcp_only = bus.discover_surface(CapabilitySurface::Mcp);
        assert_eq!(mcp_only.len(), 1);
        assert_eq!(mcp_only[0].id, "a.mcp");
    }

    #[test]
    fn dispatch_enforces_unknown_capability_and_permission_checks() {
        let mut bus = CapabilityBus::new();
        bus.register(CapabilityDescriptor::new(
            "cap.read",
            CapabilitySurface::Adapter,
            "read",
            "fs.read",
            100,
            50,
        ))
        .unwrap();

        let unknown = CapabilityRequest::new("cap.missing", "agent-1", ["fs.read".into()], 100, 50);
        assert_eq!(
            bus.dispatch(&unknown, |_| unreachable!()),
            Err(CapabilityError::UnknownCapability("cap.missing".into()))
        );

        let denied = CapabilityRequest::new("cap.read", "agent-1", ["other.perm".into()], 100, 50);
        assert_eq!(
            bus.dispatch(&denied, |_| unreachable!()),
            Err(CapabilityError::PermissionDenied("fs.read".into()))
        );
    }

    #[test]
    fn dispatch_enforces_timeout_and_budget_ceilings_and_returns_receipt() {
        let mut bus = CapabilityBus::new();
        bus.register(CapabilityDescriptor::new(
            "cap.exec",
            CapabilitySurface::A2a,
            "run",
            "a2a.run",
            100,
            50,
        ))
        .unwrap();

        let req = CapabilityRequest::new("cap.exec", "agent-a", ["a2a.run".into()], 80, 40);
        assert_eq!(
            bus.dispatch(&req, |_| Ok(CapabilityInvocation {
                output: json!(null),
                elapsed_ms: 90,
                cost_micros: 10,
            })),
            Err(CapabilityError::TimeoutExceeded {
                elapsed_ms: 90,
                limit_ms: 80,
            })
        );
        assert_eq!(
            bus.dispatch(&req, |_| Ok(CapabilityInvocation {
                output: json!(null),
                elapsed_ms: 20,
                cost_micros: 45,
            })),
            Err(CapabilityError::BudgetExceeded {
                cost_micros: 45,
                limit_micros: 40,
            })
        );
        let receipt = bus
            .dispatch(&req, |_| {
                Ok(CapabilityInvocation {
                    output: json!({"ok": true}),
                    elapsed_ms: 25,
                    cost_micros: 30,
                })
            })
            .unwrap();
        assert_eq!(receipt.capability_id, "cap.exec");
        assert_eq!(receipt.subject, "agent-a");
        assert_eq!(receipt.cost_micros, 30);
    }

    #[test]
    fn dispatch_propagates_handler_failure_and_zero_caller_timeout_as_errors() {
        let mut bus = CapabilityBus::default();
        bus.register(CapabilityDescriptor::new(
            "cap.adapter",
            CapabilitySurface::Adapter,
            "invoke",
            "adapter.run",
            100,
            50,
        ))
        .unwrap();

        let zero_timeout =
            CapabilityRequest::new("cap.adapter", "agent-z", ["adapter.run".into()], 0, 50);
        assert_eq!(
            bus.dispatch(&zero_timeout, |_| unreachable!()),
            Err(CapabilityError::TimeoutExceeded {
                elapsed_ms: 0,
                limit_ms: 0,
            })
        );

        let valid_req =
            CapabilityRequest::new("cap.adapter", "agent-z", ["adapter.run".into()], 50, 50);
        let err = bus
            .dispatch(&valid_req, |_| Err("downstream failed".into()))
            .unwrap_err();
        assert_eq!(
            err,
            CapabilityError::HandlerFailed("downstream failed".into())
        );
    }
}
