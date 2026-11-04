//! Search evidence for SearchAgent.
//!
//! An AI OS admits tools. It does not ship a weather service or a web-search
//! client. A goal is answered only by a search tool the operator has already
//! admitted on the tool bus (`brave_search`, `google_search`, `web_search`).
//! When none of those is linked, the result says so and invents nothing.

/// Prefer an admitted search tool. Never call a built-in web API.
pub fn gather_live_evidence(goal: &str, workspace: &std::path::Path) -> String {
    let mut attempts: Vec<String> = Vec::new();

    for tool in ["brave_search", "google_search", "web_search"] {
        if crate::susi_core::plane_bus::tools::exists(tool) {
            let args = serde_json::json!({ "query": goal, "q": goal });
            let out =
                crate::susi_core::plane_bus::tools::execute_autonomous_tool(tool, &args, workspace)
                    .unwrap_or_default();
            let lower = out.to_ascii_lowercase();
            if !out.trim().is_empty()
                && !lower.contains("error")
                && !lower.contains("not found")
                && !lower.contains("capability_gap")
            {
                return format!(
                    "**Live search evidence via `{tool}`**\n{out}\n- Observation time: system clock at fetch"
                );
            }
            attempts.push(format!(
                "{tool}: unusable response ({})",
                out.chars().take(120).collect::<String>()
            ));
        } else {
            attempts.push(format!("{tool}: not linked"));
        }
    }

    format!(
        "**Live search data unavailable.**\n\
         This OS does not ship a weather or web-search client. Admit a search \
         tool (`brave_search`, `google_search`, or `web_search`) to fetch live \
         evidence. Attempts:\n{}\n\
         I will not invent conditions or citations.",
        attempts
            .iter()
            .map(|a| format!("- {a}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_does_not_call_a_built_in_web_api() {
        let report = gather_live_evidence(
            "What is the current weather in Chennai, India?",
            std::path::Path::new("."),
        );
        assert!(report.contains("Live search data unavailable"));
        assert!(report.contains("does not ship a weather or web-search client"));
        assert!(!report.contains("Open-Meteo"));
        assert!(!report.contains("DuckDuckGo"));
        assert!(!report.contains("Temperature:"));
    }

    #[test]
    fn gather_lists_all_candidate_search_tools_and_refuses_to_invent_citations() {
        let report = gather_live_evidence("Latest release notes", std::path::Path::new("."));
        assert!(report.contains("Attempts:"));
        assert!(report.contains("- brave_search:"));
        assert!(report.contains("- google_search:"));
        assert!(report.contains("- web_search:"));
        assert!(report.contains("I will not invent conditions or citations."));
    }

    #[test]
    fn gather_live_evidence_with_empty_goal_still_reports_unavailable_without_panicking() {
        let report = gather_live_evidence("", std::path::Path::new("."));
        assert!(report.contains("Live search data unavailable"));
        assert!(report.contains("brave_search"));
        assert!(report.contains("google_search"));
        assert!(report.contains("web_search"));
    }

    #[test]
    fn gather_live_evidence_preserves_bullet_order_for_all_three_search_tools() {
        let report = gather_live_evidence("probe", std::path::Path::new("."));
        let brave = report.find("- brave_search:").expect("brave_search bullet");
        let google = report
            .find("- google_search:")
            .expect("google_search bullet");
        let web = report.find("- web_search:").expect("web_search bullet");
        assert!(brave < google);
        assert!(google < web);
    }

    #[test]
    fn gather_live_evidence_includes_not_linked_status_for_each_candidate_tool() {
        let report = gather_live_evidence("query", std::path::Path::new("."));
        assert!(report.contains("- brave_search: not linked"));
        assert!(report.contains("- google_search: not linked"));
        assert!(report.contains("- web_search: not linked"));
    }
}
