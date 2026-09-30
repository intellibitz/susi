//! MCP server and client features profile (tools, resources, prompts, sampling, elicitation, roots, logging, completion). Facts live in `config/ecosystem/profiles/mcp-features.json`. — offline contract tests; the facts live in `config/ecosystem/`.
#[cfg(test)]
mod tests {
    use susi_vendor_models::{eco_consistency, eco_profile, eco_store, eco_taxonomy};

    const PROFILES: &[&str] = &["mcp-features"];

    fn store() -> susi_vendor_models::eco_schema::KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_mcp_features_profiles_are_valid() {
        for pid in PROFILES {
            eco_profile::load_bundled(pid).expect("profile validates");
        }
    }

    #[test]
    fn eco_mcp_features_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_mcp_features_capabilities_are_canonical() {
        for pid in PROFILES {
            let p = eco_profile::load_bundled(pid).unwrap();
            for c in &p.capabilities {
                assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
            }
        }
    }

    #[test]
    fn eco_mcp_features_facts_cover_the_required_surface() {
        let kb = store();
        assert!(kb.entity("mcp").is_some());
        assert!(kb.entity("mcp-2025-06-18").is_some());
        assert!(kb.relations.iter().any(|r| r.kind
            == susi_vendor_models::eco_schema::RelationKind::Supersedes
            && r.from == "mcp-2025-06-18"));
    }
}
