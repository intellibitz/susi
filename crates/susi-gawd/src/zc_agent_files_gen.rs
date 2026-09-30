//! Generate agent-tool instruction files from AGENTS.md so they cannot drift.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedAgentFile {
    pub path: String,
    pub body: String,
}

/// Emit CLAUDE.md / GEMINI.md / Cursor / Copilot stubs from the shared source.
#[must_use]
pub fn generate_from_agents_md(agents_md: &str) -> Vec<GeneratedAgentFile> {
    let header = "# Generated from AGENTS.md — do not edit by hand\n\n";
    let truncated = if agents_md.len() > 4_000 {
        &agents_md[..4_000]
    } else {
        agents_md
    };
    [
        ("CLAUDE.md", "Claude Code instructions"),
        ("GEMINI.md", "Gemini CLI instructions"),
        (".cursor/rules/susi.mdc", "Cursor agent rules"),
        (
            ".github/copilot-instructions.md",
            "GitHub Copilot instructions",
        ),
    ]
    .into_iter()
    .map(|(path, label)| GeneratedAgentFile {
        path: path.into(),
        body: format!("{header}<!-- {label} -->\n\n{truncated}\n"),
    })
    .collect()
}

#[cfg(test)]
mod zc_agent_files_gen_tests {
    use super::*;

    #[test]
    fn zc_agent_files_gen_from_agents_md() {
        let files = generate_from_agents_md("# Agent Engineering Mandates\n");
        assert!(files.len() >= 4);
        assert!(files.iter().any(|f| f.path == "CLAUDE.md"));
        assert!(files.iter().all(|f| f.body.contains("AGENTS.md")));
    }
}
