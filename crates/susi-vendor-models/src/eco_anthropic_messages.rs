//! Anthropic Messages API profile — facts live in `config/ecosystem/profiles/anthropic-messages.json` plus the
//! bundled entity files it details; this module is the typed handle and the
//! offline test contract.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "anthropic-messages";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "anthropic-messages";

/// Load and validate the bundled profile.
///
/// # Errors
/// `EaiError::io`/`config` on missing or invalid profile data.
pub fn profile() -> EaiResult<Profile> {
    eco_profile::load_bundled(PROFILE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_store;
    use crate::{eco_consistency, eco_taxonomy};

    fn store() -> crate::eco_schema::KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_anthropic_messages_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_anthropic_messages_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_anthropic_messages_capabilities_are_canonical() {
        let p = profile().unwrap();
        assert!(!p.capabilities.is_empty());
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_anthropic_messages_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        assert!(!p.endpoints.is_empty());
        assert!(!p.auth.is_empty());
        assert!(!p.errors.is_empty());
        assert!(!p.rate_limits.is_empty());
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.method == "POST" && e.path == "/v1/messages"));
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.path == "/v1/messages/count_tokens"));
        assert!(p
            .auth
            .iter()
            .any(|a| a.scheme == eco_profile::AuthScheme::ApiKeyHeader && a.header == "x-api-key"));
        let s = p.streaming.as_ref().expect("messages streams");
        for ev in ["message_start", "content_block_delta", "message_stop"] {
            assert!(s.events.iter().any(|e| e == ev), "missing event {ev}");
        }
        let req = p
            .request_shapes
            .iter()
            .find(|r| r.name == "messages-request")
            .unwrap();
        for f in ["system", "tools", "thinking", "max_tokens"] {
            assert!(req.fields.iter().any(|x| x == f), "missing field {f}");
        }
        assert!(p.errors.iter().any(|e| e.code == "overloaded_error"));
        let kb = store();
        assert!(kb.entity("anthropic").is_some());
        assert!(kb.entity("anthropic-messages-2023-06-01").is_some());
    }
}
