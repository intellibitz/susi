//! Ollama native and OpenAI-compatible APIs: /api/generate|chat|embed|tags|show|pull plus the /v1 OpenAI surface, all unauthenticated on localhost.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "ollama-api";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "ollama-native";

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
    fn eco_ollama_api_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_ollama_api_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_ollama_api_capabilities_are_canonical() {
        let p = profile().unwrap();
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_ollama_api_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        let kb = store();
        assert!(kb.entity("ollama-engine").is_some());
        assert!(p.endpoints.iter().any(|e| e.path == "/api/pull"));
        assert!(p.response_shapes.iter().any(|s| s.format == "jsonl"));
    }
}
