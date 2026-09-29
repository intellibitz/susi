//! OpenAI Embeddings, Moderation, Images and Audio profile — facts live in `config/ecosystem/profiles/openai-embed-media.json` plus the
//! bundled entity files it details; this module is the typed handle and the
//! offline test contract.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "openai-embed-media";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "openai-api";

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
    fn eco_openai_embed_media_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_openai_embed_media_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_openai_embed_media_capabilities_are_canonical() {
        let p = profile().unwrap();
        assert!(!p.capabilities.is_empty());
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_openai_embed_media_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        assert!(!p.endpoints.is_empty());
        assert!(!p.auth.is_empty());
        assert!(!p.errors.is_empty());
        assert!(!p.rate_limits.is_empty());
        for path in [
            "/v1/embeddings",
            "/v1/moderations",
            "/v1/images/generations",
            "/v1/audio/speech",
            "/v1/audio/transcriptions",
        ] {
            assert!(p.endpoints.iter().any(|e| e.path == path), "missing {path}");
        }
        let emb = p
            .request_shapes
            .iter()
            .find(|s| s.name == "embed-request")
            .unwrap();
        for f in ["input", "encoding_format", "dimensions"] {
            assert!(emb.fields.iter().any(|x| x == f), "missing {f}");
        }
        for c in [
            "cap-embeddings",
            "cap-moderation",
            "cap-image-generation",
            "cap-audio",
        ] {
            assert!(p.capabilities.iter().any(|x| x == c), "missing {c}");
        }
    }
}
