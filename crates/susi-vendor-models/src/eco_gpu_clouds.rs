//! Replicate, Modal, RunPod, Lambda and CoreWeave: GPU-cloud model hosting — predictions APIs, serverless endpoints and raw GPU capacity.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "gpu-clouds";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "replicate-predictions";

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
        eco_store::load_bundled().expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_gpu_clouds_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_gpu_clouds_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_gpu_clouds_capabilities_are_canonical() {
        let p = profile().unwrap();
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_gpu_clouds_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        let kb = store();
        assert!(kb.entity("replicate-api").is_some());
        assert!(kb.entity("runpod-cloud").is_some());
        assert!(p.endpoints.iter().any(|e| e.path.contains("predictions")));
        assert!(p.endpoints.iter().any(|e| e.path.contains("runsync")));
    }

    #[test]
    fn eco_gpu_clouds_profile_matches_declared_constants_and_provenance() {
        let p = profile().expect("profile loads");
        assert_eq!(p.id, PROFILE_ID);
        assert_eq!(p.subject, SUBJECT_ID);
        assert!(!p.provenance.source.trim().is_empty());
        assert!(!p.versions.is_empty());
    }

    #[test]
    fn vc_201_055_gpu_clouds_profile_covers_reconciled_backend_entities_and_canonical_capabilities()
    {
        let p = profile().expect("profile loads");
        let kb = store();
        for entity_id in ["replicate-api", "runpod-cloud"] {
            assert!(
                kb.entity(entity_id).is_some(),
                "expected GPU cloud entity {entity_id} in bundled store"
            );
        }
        assert!(
            !p.capabilities.is_empty(),
            "gpu-clouds profile must declare canonical capabilities for reconciliation"
        );
        for cap in &p.capabilities {
            assert!(
                eco_taxonomy::canonical(cap).is_some(),
                "non-canonical capability {cap}"
            );
        }
    }
}
