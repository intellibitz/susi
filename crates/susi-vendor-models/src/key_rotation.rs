//! Expired/revoked key detection and replacement guidance (T-CLAUDE-189):
//! classify the provider's rejection, tell the user what happened and what
//! to replace — never silently retry a dead key.

/// How the vendor answered the last request with this key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyHealth {
    /// Requests succeed.
    Healthy,
    /// 401 — key invalid, revoked or malformed.
    Rejected,
    /// 403 — key valid but lacks scope/licence for this call.
    Forbidden,
    /// 429/402 — key works but quota/billing blocks the call.
    Quota,
    /// Structured `expires_at` has passed.
    Expired,
    /// No signal yet.
    Unknown,
}

/// What the key store should do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Rotation {
    /// Keep using the key.
    Keep,
    /// Replace it — carries the human-facing guidance.
    Replace { guidance: String },
    /// Key is fine; the account needs action (billing/scope).
    AccountIssue { guidance: String },
}

/// Classify a probe result into health.
///
/// `status` — HTTP status the provider returned (None = network failure,
/// which says nothing about the key). `expires_unix` — expiry claim when
/// the key format carries one (JWT-ish keys); `now` for the comparison.
pub fn classify(status: Option<u16>, expires_unix: Option<i64>, now: i64) -> KeyHealth {
    if let Some(e) = expires_unix {
        if e <= now {
            return KeyHealth::Expired;
        }
    }
    match status {
        Some(401) => KeyHealth::Rejected,
        Some(403) => KeyHealth::Forbidden,
        Some(402) | Some(429) => KeyHealth::Quota,
        Some(200..=299) => KeyHealth::Healthy,
        _ => KeyHealth::Unknown,
    }
}

/// Decide the rotation action for `vendor`'s key.
pub fn rotation_action(vendor: &str, health: KeyHealth) -> Rotation {
    match health {
        KeyHealth::Healthy | KeyHealth::Unknown => Rotation::Keep,
        KeyHealth::Expired | KeyHealth::Rejected => Rotation::Replace {
            guidance: format!(
                "the {vendor} key was rejected{} — generate a new one and run `susi keys add <key>`; the vendor is inferred automatically",
                if health == KeyHealth::Expired { " (expired)" } else { "" }
            ),
        },
        KeyHealth::Forbidden => Rotation::AccountIssue {
            guidance: format!(
                "the {vendor} key is valid but lacks permission for this call — check the key's scopes or the project's model access in the vendor console"
            ),
        },
        KeyHealth::Quota => Rotation::AccountIssue {
            guidance: format!(
                "the {vendor} account hit a quota or billing limit — the key itself is fine; check usage limits in the vendor console"
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zc_rotation_prompt_401_is_rejected_needs_replace() {
        assert_eq!(classify(Some(401), None, 100), KeyHealth::Rejected);
        match rotation_action("openai", KeyHealth::Rejected) {
            Rotation::Replace { guidance } => {
                assert!(guidance.contains("openai"));
                assert!(guidance.contains("susi keys add"));
            }
            other => panic!("expected replace, got {other:?}"),
        }
    }

    #[test]
    fn zc_rotation_prompt_expiry_claim_wins() {
        // even a 200 can't rescue a key whose own expiry has passed
        assert_eq!(classify(Some(200), Some(50), 100), KeyHealth::Expired);
        match rotation_action("anthropic", KeyHealth::Expired) {
            Rotation::Replace { guidance } => assert!(guidance.contains("expired")),
            other => panic!("expected replace, got {other:?}"),
        }
    }

    #[test]
    fn zc_rotation_prompt_403_is_scope_not_key() {
        assert_eq!(classify(Some(403), None, 100), KeyHealth::Forbidden);
        match rotation_action("google", KeyHealth::Forbidden) {
            Rotation::AccountIssue { guidance } => assert!(guidance.contains("scope")),
            other => panic!("expected account issue, got {other:?}"),
        }
    }

    #[test]
    fn zc_rotation_prompt_429_is_quota_not_key() {
        assert_eq!(classify(Some(429), None, 100), KeyHealth::Quota);
        match rotation_action("groq", KeyHealth::Quota) {
            Rotation::AccountIssue { guidance } => assert!(guidance.contains("quota")),
            other => panic!("expected account issue, got {other:?}"),
        }
    }

    #[test]
    fn zc_rotation_prompt_network_failure_keeps_key() {
        // timeout/no response says nothing about the key
        assert_eq!(classify(None, None, 100), KeyHealth::Unknown);
        assert_eq!(
            rotation_action("mistral", KeyHealth::Unknown),
            Rotation::Keep
        );
    }

    #[test]
    fn zc_rotation_prompt_healthy_keeps() {
        assert_eq!(classify(Some(200), None, 100), KeyHealth::Healthy);
        assert_eq!(
            rotation_action("openai", KeyHealth::Healthy),
            Rotation::Keep
        );
    }
}
