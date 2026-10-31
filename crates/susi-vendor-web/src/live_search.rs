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
            let out = crate::susi_core::plane_bus::tools::execute_tool(tool, &args, workspace)
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
}
