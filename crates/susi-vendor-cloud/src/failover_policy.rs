//! Policy-preserving local/cloud failover (VC-201-058).
//!
//! Routing walks the endpoint preference order and skips any candidate
//! whose state or properties would violate the request's capability,
//! privacy, deadline, or budget constraints. A fallback that cannot
//! satisfy the request is *not* a fallback — the caller gets a typed
//! `NoRoute` that names every attempt and why it failed, never a silent
//! downgrade (e.g. routing private data to a cloud endpoint because the
//! local one OOMed).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Privacy {
    /// Must not leave the local host.
    LocalOnly,
    /// May run on an operator-approved cloud endpoint.
    CloudOk,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteConstraints {
    /// Capability the endpoint must advertise (e.g. `tool:search`).
    pub needs_capability: String,
    pub privacy: Privacy,
    pub deadline_ms: u64,
    /// Request budget in cents; endpoint cost must fit.
    pub budget_cents: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointClass {
    Local,
    Cloud,
}

/// Live health of a candidate — failures are first-class inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    Up,
    /// Out of memory (local) — cannot serve.
    Oom,
    /// Provider-side outage.
    Outage,
    /// Credentials rejected.
    AuthFailed,
    /// Rate/quota exhausted for the current window.
    QuotaExhausted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    pub id: String,
    pub class: EndpointClass,
    pub capabilities: Vec<String>,
    pub est_latency_ms: u64,
    pub cost_cents: u64,
    pub state: EndpointState,
}

/// One refused candidate, kept for the explicit-failure audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub endpoint: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoRoute {
    pub refusals: Vec<Refusal>,
}

impl fmt::Display for NoRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why: Vec<String> = self
            .refusals
            .iter()
            .map(|r| format!("{}: {}", r.endpoint, r.reason))
            .collect();
        write!(f, "no policy-preserving route ({})", why.join("; "))
    }
}

impl std::error::Error for NoRoute {}

fn refuse(ep: &Endpoint, c: &RouteConstraints) -> Option<&'static str> {
    match ep.state {
        EndpointState::Up => {}
        EndpointState::Oom => return Some("local OOM"),
        EndpointState::Outage => return Some("provider outage"),
        EndpointState::AuthFailed => return Some("authentication failed"),
        EndpointState::QuotaExhausted => return Some("quota exhausted"),
    }
    if c.privacy == Privacy::LocalOnly && ep.class == EndpointClass::Cloud {
        return Some("privacy requires local");
    }
    if !ep.capabilities.iter().any(|cap| cap == &c.needs_capability) {
        return Some("missing capability");
    }
    if ep.est_latency_ms > c.deadline_ms {
        return Some("deadline");
    }
    if ep.cost_cents > c.budget_cents {
        return Some("budget");
    }
    None
}

/// First endpoint (in preference order) that satisfies every constraint.
///
/// # Errors
/// `NoRoute` listing every refused candidate and its reason — failover
/// never silently drops privacy, deadline, or budget to find a route.
pub fn route(c: &RouteConstraints, eps: &[Endpoint]) -> Result<Endpoint, NoRoute> {
    let mut refusals = Vec::new();
    for ep in eps {
        match refuse(ep, c) {
            Some(reason) => refusals.push(Refusal {
                endpoint: ep.id.clone(),
                reason,
            }),
            None => return Ok(ep.clone()),
        }
    }
    Err(NoRoute { refusals })
}
