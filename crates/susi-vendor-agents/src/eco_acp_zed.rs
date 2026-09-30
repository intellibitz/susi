//! Zed's Agent Client Protocol (ACP): JSON-RPC over stdio between editor and agent — initialize/authenticate, session/new, session/prompt and session/update streaming. — offline contract tests; the facts live in `config/ecosystem/`.
#[cfg(test)]
mod tests {
    use susi_vendor_models::{eco_consistency, eco_profile, eco_store, eco_taxonomy};

    const PROFILES: &[&str] = &["acp-zed"];

    fn store() -> susi_vendor_models::eco_schema::KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_acp_zed_profiles_are_valid() {
        for pid in PROFILES {
            eco_profile::load_bundled(pid).expect("profile validates");
        }
    }

    #[test]
    fn eco_acp_zed_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_acp_zed_capabilities_are_canonical() {
        for pid in PROFILES {
            let p = eco_profile::load_bundled(pid).unwrap();
            for c in &p.capabilities {
                assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
            }
        }
    }

    #[test]
    fn eco_acp_zed_facts_cover_the_required_surface() {
        let kb = store();
        assert!(kb.entity("acp").is_some());
        assert!(kb.entity("zed-editor").is_some());
        let p = eco_profile::load_bundled("acp-zed").unwrap();
        assert!(p
            .response_shapes
            .iter()
            .any(|s| s.fields.iter().any(|f| f.contains("session/update"))));
    }
}
