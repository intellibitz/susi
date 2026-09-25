//! Org policy engine (Swarm OS Bullet 55)
//!
//! Rules name a capability and an optional scope prefix. When several
//! rules match, deny outranks review, and review outranks allow. A
//! capability with no matching rule is allow — MAC deny-by-default is
//! enforced separately by `CapabilityPolicy`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    NeedsReview,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgRule {
    pub capability: String,
    pub scope_prefix: Option<String>,
    pub decision: PolicyDecision,
}

pub fn decide(rules: &[OrgRule], capability: &str, scope: Option<&str>) -> PolicyDecision {
    let mut matched: Option<PolicyDecision> = None;
    for rule in rules {
        if rule.capability != capability {
            continue;
        }
        let scope_ok = match (&rule.scope_prefix, scope) {
            (None, _) => true,
            (Some(prefix), Some(scope)) => scope.starts_with(prefix.as_str()),
            (Some(_), None) => false,
        };
        if !scope_ok {
            continue;
        }
        matched = Some(merge(matched, rule.decision));
    }
    matched.unwrap_or(PolicyDecision::Allow)
}

fn merge(current: Option<PolicyDecision>, next: PolicyDecision) -> PolicyDecision {
    match (current, next) {
        (Some(PolicyDecision::Deny), _) | (_, PolicyDecision::Deny) => PolicyDecision::Deny,
        (Some(PolicyDecision::NeedsReview), _) | (_, PolicyDecision::NeedsReview) => {
            PolicyDecision::NeedsReview
        }
        _ => PolicyDecision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prod_database_calls_need_review_and_a_deny_rule_wins() {
        let rules = vec![
            OrgRule {
                capability: "tool:db".into(),
                scope_prefix: Some("prod".into()),
                decision: PolicyDecision::NeedsReview,
            },
            OrgRule {
                capability: "tool:db".into(),
                scope_prefix: Some("prod/secret".into()),
                decision: PolicyDecision::Deny,
            },
        ];
        assert_eq!(
            decide(&rules, "tool:db", Some("prod/users")),
            PolicyDecision::NeedsReview
        );
        assert_eq!(
            decide(&rules, "tool:db", Some("prod/secret/keys")),
            PolicyDecision::Deny
        );
        assert_eq!(
            decide(&rules, "tool:db", Some("dev")),
            PolicyDecision::Allow
        );
    }
}
