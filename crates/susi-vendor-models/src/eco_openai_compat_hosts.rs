//! Together, Fireworks, DeepInfra and DeepSeek: hosts that expose the OpenAI chat-completions surface, with their per-host deviations recorded.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "openai-compat-hosts";
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
    fn eco_openai_compat_hosts_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_openai_compat_hosts_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_openai_compat_hosts_capabilities_are_canonical() {
        let p = profile().unwrap();
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_openai_compat_hosts_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        let kb = store();
        assert!(kb.entity("deepseek-api").is_some());
        assert!(kb.entity("together-api").is_some());
        assert!(p.deviations.iter().any(|d| d.host == "deepseek-api"));
        assert!(p.deviations.iter().any(|d| d.host == "deepinfra-api"));
    }
}
