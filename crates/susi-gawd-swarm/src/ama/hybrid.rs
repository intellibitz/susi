//! Routes a goal to coding vs assistant toolboxes by keyword matching.

use std::path::Path;
use susi_error::EaiResult;

/// Routes a goal to either the coding toolbox (`ast_analyze`) or the
/// assistant toolbox (`rag_query`) based on keyword matching.
pub struct SusiHybridAgent {
    pub coding_toolbox: Vec<String>,
    pub assistant_toolbox: Vec<String>,
}

impl Default for SusiHybridAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl SusiHybridAgent {
    pub fn new() -> Self {
        Self {
            coding_toolbox: vec![
                "ast_analyze".to_string(),
                "semantic_search".to_string(),
                "sandbox_exec".to_string(),
                "lsp_proxy".to_string(),
            ],
            assistant_toolbox: vec!["browser_automate".to_string(), "rag_query".to_string()],
        }
    }

    pub fn execute_hybrid_mission(&self, goal: &str, workspace: &Path) -> EaiResult<String> {
        let session = susi_core::capture::EvidenceSession::new(
            goal,
            workspace,
            susi_gawd_agents::security::SecurityDetector::redact,
        )
        .ok();
        let _activation = session
            .as_ref()
            .map(susi_core::capture::EvidenceSession::activate);
        let _scope = susi_core::capture::EvidenceSession::enter(session);

        eprintln!("<thinking>");
        eprintln!("[SUSI Hybrid Agent] Goal: {}", goal);

        let manifold = susi_core::manifold::IntentManifold::analyze(goal);
        eprintln!(
            "- [Intent Manifold] Scope: {:?} | Risk: {:?}",
            manifold.scope_of_impact, manifold.risk_profile
        );

        // Specialist Routing
        let result = if goal.contains("code") || goal.contains("refactor") || goal.contains("fix") {
            eprintln!("- [Specialist Route] Coding Agent Substrate Active");
            self.solve_coding_mission(goal, workspace)?
        } else {
            eprintln!("- [Specialist Route] General Assistant Substrate Active");
            self.solve_assistant_mission(goal, workspace)?
        };

        eprintln!("</thinking>\n");
        Ok(result)
    }

    fn solve_coding_mission(&self, goal: &str, workspace: &Path) -> EaiResult<String> {
        eprintln!("- [Coding Toolbox] Using: {:?}", self.coding_toolbox);
        let res = susi_tools::ToolRegistry::execute_tool(
            "ast_analyze",
            &serde_json::json!({"code": goal}),
            workspace,
        );
        Ok(format!("[HYBRID_CODING] {}", res))
    }

    fn solve_assistant_mission(&self, goal: &str, workspace: &Path) -> EaiResult<String> {
        eprintln!("- [Assistant Toolbox] Using: {:?}", self.assistant_toolbox);
        let res = susi_tools::ToolRegistry::execute_tool(
            "rag_query",
            &serde_json::json!({"query": goal}),
            workspace,
        );
        Ok(format!("[HYBRID_ASSISTANT] {}", res))
    }
}
