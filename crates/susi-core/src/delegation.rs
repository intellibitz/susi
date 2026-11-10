//! Recursive driver/engine delegation (Mandate 73).
//!
//! Any Susi component can be a *driver* (it hands work to children) and an
//! *engine* (it does work a parent handed it) at the same time, nested as deep
//! as its budget allows. A [`DelegationScope`] is the contract one level of
//! that nesting runs under, and [`DelegationScope::delegate`] is the only way
//! to open the next level. Every child scope:
//!
//! - **keeps its identity and provenance**: it knows its parent, and
//!   [`DelegationScope::path`] lists every `(component, capability)` hop from
//!   the root;
//! - **cannot amplify authority**: it holds a subset of its parent's
//!   permissions, and asking for one the parent lacks is refused;
//! - **spends from its parent's budget**: its cost budget is reserved out of
//!   the parent's remaining budget, unspent budget returns to the parent when
//!   the child is dropped, and its deadline is never later than its parent's;
//! - **is bounded**: depth and per-scope fan-out are limited, and re-entering
//!   a `(component, capability)` hop already on the ancestry is a cycle;
//! - **is cancelled with its ancestors**: cancelling a scope cancels every
//!   descendant, and [`DelegationScope::check`] reports it.
//!
//! The capability bus (`crate::capability_bus`) opens one child scope per
//! dispatch and hands it to the handler, so a handler that dispatches again is
//! a driver nested inside an engine, with these checks applied at each level.
//!
//! A [`DelegationEnvelope`] carries a scope across a process or socket
//! boundary (`os_capabilities` with a `delegation` argument). It is **not
//! signed**: it bounds cooperating components (depth, cycles, permissions and
//! budget they pass along), not an adversary, who could present a different
//! envelope just as it can present any `permission` to the bus. Authority over
//! the socket still comes from the host's MAC policy. Cancellation does not
//! cross an envelope; the remaining deadline does.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default nesting depth below a root scope.
pub const DEFAULT_MAX_DEPTH: u32 = 16;
/// Default number of children one scope may open over its lifetime.
pub const DEFAULT_MAX_CHILDREN: u32 = 256;
/// Hard ceiling on any configured depth, so an envelope cannot ask for more.
pub const MAX_DEPTH_CEILING: u32 = 256;
/// Hard ceiling on any configured fan-out.
pub const MAX_CHILDREN_CEILING: u32 = 65_536;
/// Longest component or capability identity accepted, in bytes.
pub const MAX_IDENTITY_BYTES: usize = 256;
/// Most permissions one scope may carry.
pub const MAX_PERMISSIONS: usize = 1_024;
/// Capability name recorded on a root scope's hop.
pub const ROOT_CAPABILITY: &str = "root";
/// Schema tag of a serialized [`DelegationEnvelope`].
pub const ENVELOPE_SCHEMA: &str = "susi-delegation/v1";

/// Depth and fan-out bounds, inherited unchanged by every descendant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationLimits {
    /// Deepest level below the root a child may be opened at.
    pub max_depth: u32,
    /// Children one scope may open over its lifetime (not only concurrently),
    /// so a loop that keeps delegating is stopped, not merely slowed.
    pub max_children: u32,
}

impl Default for DelegationLimits {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_children: DEFAULT_MAX_CHILDREN,
        }
    }
}

impl DelegationLimits {
    fn validate(self) -> Result<Self, DelegationError> {
        if self.max_depth > MAX_DEPTH_CEILING || self.max_children > MAX_CHILDREN_CEILING {
            return Err(DelegationError::InvalidLimits(format!(
                "max_depth {} (ceiling {MAX_DEPTH_CEILING}) and max_children {} (ceiling {MAX_CHILDREN_CEILING})",
                self.max_depth, self.max_children
            )));
        }
        Ok(self)
    }
}

/// One level of a delegation path: who runs, and which capability it runs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DelegationHop {
    /// Component identity (agent, driver, engine, service), e.g. `opus@laptop-2/cloud`.
    pub component: String,
    /// Capability the component exercises at this level.
    pub capability: String,
}

impl DelegationHop {
    /// A validated hop: both identities non-empty, at most
    /// [`MAX_IDENTITY_BYTES`] and free of control characters.
    pub fn new(
        component: impl Into<String>,
        capability: impl Into<String>,
    ) -> Result<Self, DelegationError> {
        let hop = Self {
            component: component.into(),
            capability: capability.into(),
        };
        validate_identity("component", &hop.component)?;
        validate_identity("capability", &hop.capability)?;
        Ok(hop)
    }
}

impl fmt::Display for DelegationHop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.component, self.capability)
    }
}

fn validate_identity(what: &str, value: &str) -> Result<(), DelegationError> {
    if value.trim().is_empty()
        || value.len() > MAX_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DelegationError::InvalidIdentity(format!(
            "{what} must be 1..={MAX_IDENTITY_BYTES} bytes without control characters"
        )));
    }
    Ok(())
}

/// A typed refusal to open, continue or charge a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationError {
    /// A component or capability identity is empty, too long or malformed.
    InvalidIdentity(String),
    /// Limits exceed the hard ceilings.
    InvalidLimits(String),
    /// The scope or one of its ancestors was cancelled.
    Cancelled(String),
    /// The scope's deadline (inherited from its ancestors) has passed.
    DeadlineExceeded(String),
    /// Opening the child would exceed the inherited depth limit.
    DepthExceeded { depth: u32, limit: u32 },
    /// The parent has already opened its allowed number of children.
    FanOutExceeded { limit: u32 },
    /// The requested hop is already on the ancestry: a cycle.
    Cycle {
        hop: DelegationHop,
        path: Vec<DelegationHop>,
    },
    /// The child asked for permissions its parent does not hold.
    PermissionAmplification { missing: Vec<String> },
    /// The request needs more budget than the scope has left.
    BudgetExceeded {
        requested_micros: u64,
        available_micros: u64,
    },
    /// A serialized envelope is malformed or inconsistent.
    InvalidEnvelope(String),
}

impl fmt::Display for DelegationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity(why) => write!(f, "invalid delegation identity: {why}"),
            Self::InvalidLimits(why) => write!(f, "invalid delegation limits: {why}"),
            Self::Cancelled(at) => write!(f, "delegation cancelled at {at}"),
            Self::DeadlineExceeded(at) => write!(f, "delegation deadline passed at {at}"),
            Self::DepthExceeded { depth, limit } => {
                write!(f, "delegation depth {depth} exceeds limit {limit}")
            }
            Self::FanOutExceeded { limit } => {
                write!(f, "delegation fan-out exceeds {limit} children")
            }
            Self::Cycle { hop, path } => {
                let path: Vec<String> = path.iter().map(ToString::to_string).collect();
                write!(f, "delegation cycle: {hop} already on {}", path.join(" > "))
            }
            Self::PermissionAmplification { missing } => write!(
                f,
                "delegation would amplify permissions: parent lacks {}",
                missing.join(", ")
            ),
            Self::BudgetExceeded {
                requested_micros,
                available_micros,
            } => write!(
                f,
                "delegation budget {requested_micros} micros exceeds {available_micros} available"
            ),
            Self::InvalidEnvelope(why) => write!(f, "invalid delegation envelope: {why}"),
        }
    }
}

impl std::error::Error for DelegationError {}

/// What a parent grants one child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationRequest {
    /// The child's identity and the capability it will exercise.
    pub hop: DelegationHop,
    /// Permissions the child needs: must be a subset of the parent's.
    pub permissions: BTreeSet<String>,
    /// Cost budget reserved from the parent's remaining budget.
    pub budget_micros: u64,
    /// Time allowed; the child's deadline is the earlier of this and the parent's.
    pub timeout_ms: u64,
}

#[derive(Debug)]
struct Node {
    hop: DelegationHop,
    parent: Option<Arc<Node>>,
    depth: u32,
    permissions: BTreeSet<String>,
    deadline: Option<Instant>,
    remaining_micros: AtomicU64,
    spent_micros: AtomicU64,
    children: AtomicU32,
    cancelled: AtomicBool,
    limits: DelegationLimits,
}

impl Drop for Node {
    /// Unspent budget goes back to the parent once nothing refers to this
    /// level any more (its own children hold it alive until they finish).
    fn drop(&mut self) {
        if let Some(parent) = &self.parent {
            let unspent = *self.remaining_micros.get_mut();
            add_saturating(&parent.remaining_micros, unspent);
        }
    }
}

fn add_saturating(cell: &AtomicU64, amount: u64) {
    let mut current = cell.load(Ordering::Acquire);
    loop {
        let next = current.saturating_add(amount);
        match cell.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

/// Take `amount` from `cell` only if it is all there.
fn take(cell: &AtomicU64, amount: u64) -> Result<(), u64> {
    let mut current = cell.load(Ordering::Acquire);
    loop {
        if amount > current {
            return Err(current);
        }
        match cell.compare_exchange_weak(
            current,
            current - amount,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok(()),
            Err(actual) => current = actual,
        }
    }
}

/// One level of recursive delegation. Cheap to clone; clones share the level.
#[derive(Debug, Clone)]
pub struct DelegationScope {
    node: Arc<Node>,
}

impl DelegationScope {
    /// A root scope: the authority a session, CLI call or socket request
    /// starts with. `timeout_ms` of `None` means no deadline.
    pub fn root(
        hop: DelegationHop,
        permissions: impl IntoIterator<Item = String>,
        budget_micros: u64,
        timeout_ms: Option<u64>,
        limits: DelegationLimits,
    ) -> Result<Self, DelegationError> {
        let hop = DelegationHop::new(hop.component, hop.capability)?;
        let limits = limits.validate()?;
        let permissions = collect_permissions(permissions)?;
        Ok(Self {
            node: Arc::new(Node {
                hop,
                parent: None,
                depth: 0,
                permissions,
                deadline: timeout_ms.and_then(deadline_after),
                remaining_micros: AtomicU64::new(budget_micros),
                spent_micros: AtomicU64::new(0),
                children: AtomicU32::new(0),
                cancelled: AtomicBool::new(false),
                limits,
            }),
        })
    }

    /// Open a child level. Every refusal leaves this scope unchanged.
    pub fn delegate(&self, request: DelegationRequest) -> Result<Self, DelegationError> {
        self.check()?;
        let hop = DelegationHop::new(request.hop.component, request.hop.capability)?;
        let parent = &self.node;
        let depth = parent.depth.saturating_add(1);
        if depth > parent.limits.max_depth {
            return Err(DelegationError::DepthExceeded {
                depth,
                limit: parent.limits.max_depth,
            });
        }
        let path = self.path();
        if path.contains(&hop) {
            return Err(DelegationError::Cycle { hop, path });
        }
        let permissions = collect_permissions(request.permissions)?;
        let missing: Vec<String> = permissions
            .difference(&parent.permissions)
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(DelegationError::PermissionAmplification { missing });
        }
        let mut opened = parent.children.load(Ordering::Acquire);
        loop {
            if opened >= parent.limits.max_children {
                return Err(DelegationError::FanOutExceeded {
                    limit: parent.limits.max_children,
                });
            }
            match parent.children.compare_exchange_weak(
                opened,
                opened + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => opened = actual,
            }
        }
        if let Err(available) = take(&parent.remaining_micros, request.budget_micros) {
            // The slot was not used: give it back so a refusal changes nothing.
            parent.children.fetch_sub(1, Ordering::AcqRel);
            return Err(DelegationError::BudgetExceeded {
                requested_micros: request.budget_micros,
                available_micros: available,
            });
        }
        let deadline = match (parent.deadline, deadline_after(request.timeout_ms)) {
            (Some(theirs), Some(ours)) => Some(theirs.min(ours)),
            (Some(theirs), None) => Some(theirs),
            (None, ours) => ours,
        };
        Ok(Self {
            node: Arc::new(Node {
                hop,
                parent: Some(Arc::clone(parent)),
                depth,
                permissions,
                deadline,
                remaining_micros: AtomicU64::new(request.budget_micros),
                spent_micros: AtomicU64::new(0),
                children: AtomicU32::new(0),
                cancelled: AtomicBool::new(false),
                limits: parent.limits,
            }),
        })
    }

    /// Spend `micros` of this scope's own remaining budget.
    pub fn charge(&self, micros: u64) -> Result<(), DelegationError> {
        take(&self.node.remaining_micros, micros).map_err(|available| {
            DelegationError::BudgetExceeded {
                requested_micros: micros,
                available_micros: available,
            }
        })?;
        add_saturating(&self.node.spent_micros, micros);
        Ok(())
    }

    /// Cancel this level and, through it, every descendant.
    pub fn cancel(&self) {
        self.node.cancelled.store(true, Ordering::Release);
    }

    /// The first cancelled scope on the ancestry, this one included.
    fn cancelled_at(&self) -> Option<&DelegationHop> {
        let mut node = Some(&self.node);
        while let Some(current) = node {
            if current.cancelled.load(Ordering::Acquire) {
                return Some(&current.hop);
            }
            node = current.parent.as_ref();
        }
        None
    }

    /// Whether this scope or an ancestor was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled_at().is_some()
    }

    /// Refuse to continue once cancelled; cancellation is reported first.
    pub fn check_cancelled(&self) -> Result<(), DelegationError> {
        match self.cancelled_at() {
            Some(hop) => Err(DelegationError::Cancelled(hop.to_string())),
            None => Ok(()),
        }
    }

    /// Refuse to continue once cancelled or past the inherited deadline.
    pub fn check(&self) -> Result<(), DelegationError> {
        self.check_cancelled()?;
        if self
            .node
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(DelegationError::DeadlineExceeded(self.node.hop.to_string()));
        }
        Ok(())
    }

    /// Every hop from the root down to this scope.
    #[must_use]
    pub fn path(&self) -> Vec<DelegationHop> {
        let mut hops = Vec::with_capacity(self.node.depth as usize + 1);
        let mut node = Some(&self.node);
        while let Some(current) = node {
            hops.push(current.hop.clone());
            node = current.parent.as_ref();
        }
        hops.reverse();
        hops
    }

    /// This level's hop.
    #[must_use]
    pub fn hop(&self) -> &DelegationHop {
        &self.node.hop
    }

    /// Levels below the root (the root is 0).
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.node.depth
    }

    /// Permissions this level holds.
    #[must_use]
    pub fn permissions(&self) -> &BTreeSet<String> {
        &self.node.permissions
    }

    /// Budget not yet spent or reserved by open children.
    #[must_use]
    pub fn remaining_micros(&self) -> u64 {
        self.node.remaining_micros.load(Ordering::Acquire)
    }

    /// Budget this level charged itself (children's spending is theirs).
    #[must_use]
    pub fn spent_micros(&self) -> u64 {
        self.node.spent_micros.load(Ordering::Acquire)
    }

    /// Time left before the inherited deadline; `None` when there is none.
    #[must_use]
    pub fn remaining_time(&self) -> Option<Duration> {
        self.node
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// The bounds every descendant inherits.
    #[must_use]
    pub fn limits(&self) -> DelegationLimits {
        self.node.limits
    }

    /// Serialize this level so a component across a process or socket
    /// boundary can continue the nesting under the same bounds.
    #[must_use]
    pub fn envelope(&self) -> DelegationEnvelope {
        DelegationEnvelope {
            schema: ENVELOPE_SCHEMA.to_string(),
            path: self.path(),
            permissions: self.node.permissions.iter().cloned().collect(),
            budget_micros: self.remaining_micros(),
            timeout_ms: self
                .remaining_time()
                .map(|left| u64::try_from(left.as_millis()).unwrap_or(u64::MAX)),
            limits: self.node.limits,
        }
    }

    /// Continue a nesting received over a boundary. The ancestry is rebuilt
    /// so depth and cycle checks keep counting from where the sender was;
    /// the last hop holds the envelope's permissions, budget and deadline.
    pub fn from_envelope(envelope: &DelegationEnvelope) -> Result<Self, DelegationError> {
        if envelope.schema != ENVELOPE_SCHEMA {
            return Err(DelegationError::InvalidEnvelope(format!(
                "schema `{}` is not `{ENVELOPE_SCHEMA}`",
                envelope.schema
            )));
        }
        let limits = envelope.limits.validate()?;
        let Some((last, ancestors)) = envelope.path.split_last() else {
            return Err(DelegationError::InvalidEnvelope("empty path".into()));
        };
        let depth = u32::try_from(ancestors.len()).unwrap_or(u32::MAX);
        if depth > limits.max_depth {
            return Err(DelegationError::DepthExceeded {
                depth,
                limit: limits.max_depth,
            });
        }
        let mut seen = BTreeSet::new();
        for hop in &envelope.path {
            DelegationHop::new(hop.component.clone(), hop.capability.clone())?;
            if !seen.insert(hop) {
                return Err(DelegationError::Cycle {
                    hop: hop.clone(),
                    path: envelope.path.clone(),
                });
            }
        }
        let permissions = collect_permissions(envelope.permissions.iter().cloned())?;
        let mut parent: Option<Arc<Node>> = None;
        for (level, hop) in ancestors.iter().enumerate() {
            parent = Some(Arc::new(Node {
                hop: hop.clone(),
                parent: parent.take(),
                depth: u32::try_from(level).unwrap_or(u32::MAX),
                permissions: permissions.clone(),
                deadline: None,
                remaining_micros: AtomicU64::new(0),
                spent_micros: AtomicU64::new(0),
                children: AtomicU32::new(1),
                cancelled: AtomicBool::new(false),
                limits,
            }));
        }
        Ok(Self {
            node: Arc::new(Node {
                hop: last.clone(),
                parent,
                depth,
                permissions,
                deadline: envelope.timeout_ms.and_then(deadline_after),
                remaining_micros: AtomicU64::new(envelope.budget_micros),
                spent_micros: AtomicU64::new(0),
                children: AtomicU32::new(0),
                cancelled: AtomicBool::new(false),
                limits,
            }),
        })
    }
}

fn deadline_after(timeout_ms: u64) -> Option<Instant> {
    Instant::now().checked_add(Duration::from_millis(timeout_ms))
}

fn collect_permissions(
    permissions: impl IntoIterator<Item = String>,
) -> Result<BTreeSet<String>, DelegationError> {
    let mut set = BTreeSet::new();
    for permission in permissions {
        validate_identity("permission", &permission)?;
        set.insert(permission);
        if set.len() > MAX_PERMISSIONS {
            return Err(DelegationError::InvalidIdentity(format!(
                "more than {MAX_PERMISSIONS} permissions"
            )));
        }
    }
    Ok(set)
}

/// A delegation scope in transit across a process or socket boundary.
/// Unsigned: see the module documentation for what it does and does not bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationEnvelope {
    /// Always [`ENVELOPE_SCHEMA`].
    pub schema: String,
    /// Hops from the root to the sending level, root first.
    pub path: Vec<DelegationHop>,
    /// Permissions the sending level holds.
    pub permissions: Vec<String>,
    /// Budget the sending level had left.
    pub budget_micros: u64,
    /// Time the sending level had left; absent when it had no deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Bounds inherited by every further level.
    pub limits: DelegationLimits,
}
