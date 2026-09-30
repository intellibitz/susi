//! T-CLAUDE-323 — OWASP LLM Top 10 mapping checks (offline; facts in `config/ecosystem/`).
#[cfg(test)]
mod eco_owasp_llm {
    use susi_vendor_models::{eco_consistency, eco_profile, eco_store};

    fn profile_json() -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/ecosystem/profiles/owasp-llm-top10.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn eco_owasp_llm_profile_is_valid() {
        let p = eco_profile::load_bundled("owasp-llm-top10").expect("profile validates");
        assert_eq!(p.subject, "owasp-llm-top10");
    }

    #[test]
    fn eco_owasp_llm_entities_are_in_the_store() {
        let kb = eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads");
        assert!(kb.entity("owasp").is_some());
        assert!(kb.entity("owasp-llm-top10").is_some());
        assert!(kb.entity("owasp-llm-2025").is_some());
        let issues = eco_consistency::check(&kb);
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_owasp_llm_lists_all_ten_2025_risks() {
        let p = profile_json();
        let ids: Vec<_> = p["request_shapes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 10, "{ids:?}");
        for n in 1..=10 {
            let prefix = format!("llm{n:02}-");
            assert!(ids.iter().any(|id| id.starts_with(&prefix)), "{prefix}");
        }
    }

    #[test]
    fn eco_owasp_llm_mapped_tests_cover_documented_risks() {
        let p = profile_json();
        let mappings = p["mappings"].as_array().unwrap();
        let covered: Vec<_> = mappings
            .iter()
            .map(|m| m["risk"].as_str().unwrap())
            .collect();
        for risk in ["LLM01", "LLM02", "LLM03", "LLM06", "LLM10"] {
            assert!(covered.contains(&risk), "{risk} not mapped");
        }
        for m in mappings {
            assert!(!m["susi_test"].as_str().unwrap().is_empty());
            assert!(!m["coverage"].as_str().unwrap().is_empty());
        }
        // honestly notes the unmapped remainder instead of over-claiming
        let text = serde_json::to_string(&p).unwrap();
        assert!(text.contains("LLM04") && text.contains("unmapped"));
    }
}
