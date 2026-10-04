//! Per-mission egress allow-list enforcement.
//!
//! A mission declares the hosts it is allowed to reach. Every egress attempt
//! is checked against that mission's `EgressPolicy`; a host that the policy
//! does not name is refused, and the refusal is recorded together with the
//! mission that attempted it and the reason it was refused.
//!
//! Provider traffic must remain possible under the default policy, so the
//! allow-list is data a mission declares — it is *not* a global switch that
//! defaults to closed. [`EgressPolicy::default`] is therefore permissive, and
//! [`EgressPolicy::from_allowlist`] seeds a closed policy from the
//! vendor-derived host set in [`crate::zc_egress_allowlist`].

use crate::zc_egress_allowlist::EgressAllowlist;
use std::collections::BTreeSet;
use std::fmt;

/// Why an egress attempt was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// The host is not named by the mission's policy.
    HostNotAllowed,
    /// The policy names no hosts and is not permissive.
    PolicyEmpty,
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DenyReason::HostNotAllowed => write!(f, "host not named by mission policy"),
            DenyReason::PolicyEmpty => write!(f, "mission policy names no hosts"),
        }
    }
}

/// A record of a refused egress attempt.
///
/// Carries the mission that attempted the egress, the host it tried to reach,
/// and the reason the attempt was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRefusal {
    /// The mission that attempted the egress.
    pub mission: String,
    /// The host the mission tried to reach.
    pub host: String,
    /// Why the attempt was refused.
    pub reason: DenyReason,
}

impl fmt::Display for EgressRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "egress refused for mission `{}` to `{}`: {}",
            self.mission, self.host, self.reason
        )
    }
}

/// The allow-list a single mission declares.
///
/// Hosts are matched exactly. A host of the form `*.example.com` additionally
/// matches any subdomain of `example.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPolicy {
    hosts: BTreeSet<String>,
    permissive: bool,
}

impl Default for EgressPolicy {
    /// The default policy is permissive so that provider traffic stays
    /// possible without every mission having to opt in.
    fn default() -> Self {
        Self::permissive()
    }
}

impl EgressPolicy {
    /// A policy that names no hosts and refuses everything.
    ///
    /// This is the *opt-in to restriction* policy: a mission that wants a
    /// closed allow-list starts from here and declares the hosts it needs.
    pub fn deny_all() -> Self {
        Self {
            hosts: BTreeSet::new(),
            permissive: false,
        }
    }

    /// A policy that permits any host.
    pub fn permissive() -> Self {
        Self {
            hosts: BTreeSet::new(),
            permissive: true,
        }
    }

    /// Build a closed policy from a vendor-derived
    /// [`EgressAllowlist`](crate::zc_egress_allowlist::EgressAllowlist).
    ///
    /// This lets a mission reuse the vendor → host mapping instead of
    /// hard-coding host names, while still being an explicit allow-list.
    pub fn from_allowlist(list: &EgressAllowlist) -> Self {
        Self {
            hosts: list.hosts.clone(),
            permissive: false,
        }
    }

    /// Declare `host` as allowed, returning the updated policy.
    ///
    /// Builder style so a mission can chain declarations:
    /// `EgressPolicy::deny_all().allow_host("api.example.com")`.
    pub fn allow_host(mut self, host: impl Into<String>) -> Self {
        self.hosts.insert(host.into());
        self
    }

    /// Whether this policy is permissive.
    pub fn is_permissive(&self) -> bool {
        self.permissive
    }

    /// The hosts this policy names.
    pub fn hosts(&self) -> impl Iterator<Item = &str> {
        self.hosts.iter().map(String::as_str)
    }

    /// Whether `host` is permitted by this policy.
    pub fn allows(&self, host: &str) -> bool {
        if self.permissive {
            return true;
        }
        if self.hosts.contains(host) {
            return true;
        }
        self.hosts
            .iter()
            .any(|allowed| match allowed.strip_prefix("*.") {
                Some(suffix) => {
                    host.len() > suffix.len()
                        && host.ends_with(suffix)
                        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
                }
                None => false,
            })
    }
}

/// A gate that checks egress attempts against a policy and records refusals.
#[derive(Debug, Clone)]
pub struct EgressGate {
    policy: EgressPolicy,
    refusals: Vec<EgressRefusal>,
}

impl EgressGate {
    /// Build a gate from a mission's declared policy.
    pub fn new(policy: EgressPolicy) -> Self {
        Self {
            policy,
            refusals: Vec::new(),
        }
    }

    /// The policy backing this gate.
    pub fn policy(&self) -> &EgressPolicy {
        &self.policy
    }

    /// Attempt egress to `host` on behalf of `mission`.
    ///
    /// On success returns `Ok(())`. On refusal records the attempt and returns
    /// the reason it was refused.
    pub fn authorize(&mut self, mission: &str, host: &str) -> Result<(), DenyReason> {
        if self.policy.allows(host) {
            return Ok(());
        }
        let reason = if self.policy.hosts.is_empty() && !self.policy.permissive {
            DenyReason::PolicyEmpty
        } else {
            DenyReason::HostNotAllowed
        };
        self.refusals.push(EgressRefusal {
            mission: mission.to_owned(),
            host: host.to_owned(),
            reason,
        });
        Err(reason)
    }

    /// Every refusal recorded so far, in the order it occurred.
    pub fn refusals(&self) -> &[EgressRefusal] {
        &self.refusals
    }

    /// Take the recorded refusals, leaving the gate with an empty log.
    pub fn take_refusals(&mut self) -> Vec<EgressRefusal> {
        std::mem::take(&mut self.refusals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_allow_list_default_policy_permits_provider_traffic() {
        let mut gate = EgressGate::new(EgressPolicy::default());
        assert!(gate.policy().is_permissive());
        assert_eq!(gate.authorize("mission-a", "api.openai.com"), Ok(()));
        assert!(gate.refusals().is_empty());
    }

    #[test]
    fn egress_allow_list_allows_declared_host_only() {
        let policy = EgressPolicy::deny_all().allow_host("api.example.com");
        let mut gate = EgressGate::new(policy);

        assert_eq!(gate.authorize("mission-a", "api.example.com"), Ok(()));
        assert_eq!(
            gate.authorize("mission-a", "api.other.com"),
            Err(DenyReason::HostNotAllowed)
        );
        // The refusal is recorded, not just returned.
        assert_eq!(gate.refusals().len(), 1);
    }

    #[test]
    fn egress_allow_list_records_mission_and_reason() {
        let policy = EgressPolicy::deny_all().allow_host("api.example.com");
        let mut gate = EgressGate::new(policy);

        let outcome = gate.authorize("mission-beta", "exfil.example.net");
        assert_eq!(outcome, Err(DenyReason::HostNotAllowed));

        let refusals = gate.refusals();
        assert_eq!(refusals.len(), 1);
        let refusal = &refusals[0];
        assert_eq!(refusal.mission, "mission-beta");
        assert_eq!(refusal.host, "exfil.example.net");
        assert_eq!(refusal.reason, DenyReason::HostNotAllowed);
    }

    #[test]
    fn egress_allow_list_empty_policy_denies_with_policy_empty() {
        let mut gate = EgressGate::new(EgressPolicy::deny_all());

        assert_eq!(
            gate.authorize("mission-a", "api.example.com"),
            Err(DenyReason::PolicyEmpty)
        );
        assert_eq!(gate.refusals().len(), 1);
        assert_eq!(gate.refusals()[0].reason, DenyReason::PolicyEmpty);
    }

    #[test]
    fn egress_allow_list_wildcard_matches_subdomains() {
        let policy = EgressPolicy::deny_all().allow_host("*.example.com");
        let mut gate = EgressGate::new(policy);

        assert_eq!(gate.authorize("mission-a", "a.example.com"), Ok(()));
        assert_eq!(gate.authorize("mission-a", "deep.a.example.com"), Ok(()));
        // The bare apex is not a subdomain.
        assert_eq!(
            gate.authorize("mission-a", "example.com"),
            Err(DenyReason::HostNotAllowed)
        );
        // A lookalike suffix must not slip through.
        assert_eq!(
            gate.authorize("mission-a", "notexample.com"),
            Err(DenyReason::HostNotAllowed)
        );
        assert_eq!(gate.refusals().len(), 2);
    }

    #[test]
    fn egress_allow_list_from_vendors_permits_only_named_provider_hosts() {
        use crate::zc_egress_allowlist::allowlist_from_vendors;

        let vendors = allowlist_from_vendors(&["openai", "anthropic"]);
        let mut gate = EgressGate::new(EgressPolicy::from_allowlist(&vendors));

        assert_eq!(gate.authorize("mission-a", "api.openai.com"), Ok(()));
        assert_eq!(gate.authorize("mission-a", "api.anthropic.com"), Ok(()));
        assert_eq!(
            gate.authorize("mission-a", "evil.example"),
            Err(DenyReason::HostNotAllowed)
        );
        assert_eq!(gate.refusals().len(), 1);
    }

    #[test]
    fn egress_allow_list_take_refusals_drains_the_log() {
        let policy = EgressPolicy::deny_all().allow_host("api.example.com");
        let mut gate = EgressGate::new(policy);

        let _ = gate.authorize("mission-a", "denied.example.net");
        let drained = gate.take_refusals();

        assert_eq!(drained.len(), 1);
        assert!(gate.refusals().is_empty());
    }
}
