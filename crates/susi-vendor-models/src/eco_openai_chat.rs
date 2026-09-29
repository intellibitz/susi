//! OpenAI Chat Completions profile — the de-facto-compatible API the whole
//! ecosystem converged on. The facts live in
//! `config/ecosystem/profiles/openai-chat-completions.json` plus the entity
//! files it details; this module is the typed handle and its offline test
//! contract.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "openai-chat-completions";
/// Knowledge-base protocol entity this profile details.
pub const PROTOCOL_ID: &str = "openai-chat-completions";

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
    use crate::eco_consistency;
    use crate::eco_schema::EntityKind;
    use crate::eco_store;
    use crate::eco_taxonomy;

    fn store() -> crate::eco_schema::KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_openai_chat_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_openai_chat_store_is_consistent() {
        let kb = store();
        let issues = eco_consistency::check(&kb);
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_openai_chat_subject_is_a_protocol_in_the_store() {
        let kb = store();
        let e = kb.entity(PROTOCOL_ID).expect("protocol entity exists");
        assert_eq!(e.kind(), EntityKind::Protocol);
        // openai-api implements the concrete version.
        assert!(kb.relations.iter().any(|r| r.from == "openai-api"
            && r.kind == crate::eco_schema::RelationKind::ImplementsVersion
            && r.to == "openai-chat-completions-2024-06"));
    }

    #[test]
    fn eco_openai_chat_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.method == "POST" && e.path == "/v1/chat/completions"));
        assert!(p.endpoints.iter().any(|e| e.streaming));
        assert_eq!(p.auth[0].scheme, eco_profile::AuthScheme::Bearer);
        assert!(p.auth[0].env.iter().any(|v| v == "OPENAI_API_KEY"));
        // Streaming is SSE with the [DONE] terminator.
        let s = p.streaming.as_ref().expect("chat streams");
        assert_eq!(s.format, "sse");
        assert!(s.events.iter().any(|e| e.contains("[DONE]")));
        // Rate-limit headers and the 429 error are documented.
        assert!(p
            .rate_limits
            .iter()
            .any(|r| r.signal.starts_with("x-ratelimit")));
        assert!(p.errors.iter().any(|e| e.status == "429" && e.retriable));
        // Compatible-host deviations are recorded, not assumed away.
        assert!(!p.deviations.is_empty());
        // The request shape carries the chat-completions load-bearing fields.
        let req = p
            .request_shapes
            .iter()
            .find(|s| s.name == "chat-request")
            .unwrap();
        for f in ["model", "messages", "tools", "response_format", "stream"] {
            assert!(req.fields.iter().any(|x| x == f), "missing field {f}");
        }
    }

    #[test]
    fn eco_openai_chat_capabilities_are_canonical() {
        let p = profile().unwrap();
        for c in &p.capabilities {
            assert!(
                eco_taxonomy::canonical(c).is_some(),
                "{c} is not in the capability taxonomy"
            );
        }
        // Chat Completions must carry chat + tools at minimum.
        assert!(p.capabilities.iter().any(|c| c == "cap-chat"));
        assert!(p.capabilities.iter().any(|c| c == "cap-tool-calling"));
    }
}
