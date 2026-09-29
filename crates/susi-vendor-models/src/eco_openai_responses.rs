//! OpenAI Responses API profile — the items/state successor to Chat
//! Completions. Facts live in
//! `config/ecosystem/profiles/openai-responses.json`; the module is the
//! typed handle plus the offline contract.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "openai-responses";
/// Knowledge-base protocol entity this profile details.
pub const PROTOCOL_ID: &str = "openai-responses";

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
    use crate::eco_schema::RelationKind;
    use crate::eco_store;
    use crate::{eco_consistency, eco_taxonomy};

    fn store() -> crate::eco_schema::KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_openai_responses_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_openai_responses_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_openai_responses_facts_cover_items_state_and_events() {
        let p = profile().unwrap();
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.method == "POST" && e.path == "/v1/responses"));
        // Retrieval + deletion of stored responses are part of the surface.
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.method == "GET" && e.path.contains("{response_id}")));
        assert!(p.endpoints.iter().any(|e| e.method == "DELETE"));
        // Streaming is semantic SSE events, not raw chunks.
        let s = p.streaming.as_ref().expect("responses streams");
        assert_eq!(s.format, "sse");
        assert!(s.events.iter().any(|e| e == "response.completed"));
        // Items + conversation state are the differentiators.
        let req = p
            .request_shapes
            .iter()
            .find(|r| r.name == "responses-request")
            .unwrap();
        for f in ["input", "previous_response_id", "store", "tools"] {
            assert!(req.fields.iter().any(|x| x == f), "missing {f}");
        }
        // The chat<->responses differences are recorded as deviations.
        assert!(p
            .deviations
            .iter()
            .any(|d| d.host == "openai-chat-completions" && d.field == "state"));
    }

    #[test]
    fn eco_openai_responses_capabilities_are_canonical() {
        let p = profile().unwrap();
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
        assert!(p.capabilities.iter().any(|c| c == "cap-reasoning"));
    }

    #[test]
    fn eco_openai_responses_wired_in_the_store() {
        let kb = store();
        assert!(kb.entity(PROTOCOL_ID).is_some());
        assert!(kb.relations.iter().any(|r| r.from == "openai-api"
            && r.kind == RelationKind::ImplementsVersion
            && r.to == "openai-responses-2025-03"));
    }
}
