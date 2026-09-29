//! OpenAI Realtime profile (WebSocket/WebRTC) — facts live in `config/ecosystem/profiles/openai-realtime.json` plus the
//! bundled entity files it details; this module is the typed handle and the
//! offline test contract.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "openai-realtime";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "openai-realtime";

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
    fn eco_openai_realtime_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_openai_realtime_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_openai_realtime_capabilities_are_canonical() {
        let p = profile().unwrap();
        assert!(!p.capabilities.is_empty());
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_openai_realtime_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        assert!(!p.endpoints.is_empty());
        assert!(!p.auth.is_empty());
        assert!(!p.errors.is_empty());
        assert!(!p.rate_limits.is_empty());
        assert!(p.endpoints.iter().any(|e| e.method == "WSS"));
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.path == "/v1/realtime/sessions"));
        assert!(p
            .auth
            .iter()
            .any(|a| a.scheme == eco_profile::AuthScheme::SessionToken));
        let s = p.streaming.as_ref().expect("realtime streams");
        assert!(s
            .events
            .iter()
            .any(|e| e == "input_audio_buffer.speech_started"));
        assert!(s.events.iter().any(|e| e == "response.done"));
        assert!(p.capabilities.iter().any(|c| c == "cap-realtime"));
        assert!(p.capabilities.iter().any(|c| c == "cap-audio"));
        let kb = store();
        assert!(kb.entity("openai-realtime-2025-08").is_some());
    }
}
