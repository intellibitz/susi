//! CrewAI E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "crewai";
pub const CONFIG_ENV: &str = "SUSI_CREWAI_CONFIG";
pub const IMPORT_NAME: &str = "crewai";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install crewai",
    documentation: "https://docs.crewai.com/",
    scaffold_subdir: "crewai",
    example_script: r#"#!/usr/bin/env python3
"""Minimal CrewAI entry used by SUSI. Replace with your real crew."""
from __future__ import annotations

import json
import os
import sys

from crewai import Agent, Crew, Process, Task


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    agent = Agent(
        role="SusiCrewMate",
        goal="Complete the user task concisely and correctly",
        backstory="You are a focused coding agent invoked by SUSI.",
        verbose=False,
        allow_delegation=False,
    )
    task = Task(description=prompt, expected_output="A concise answer", agent=agent)
    crew = Crew(agents=[agent], tasks=[task], process=Process.sequential, verbose=False)
    output = crew.kickoff()
    print(json.dumps({"prompt": prompt, "answer": str(output)}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    example_filename: "agent.py",
    credential_envs: &["OPENAI_API_KEY", "OPENROUTER_API_KEY", "ANTHROPIC_API_KEY"],
    credential_hint: "export OPENAI_API_KEY=… (or OPENROUTER_API_KEY / ANTHROPIC_API_KEY)",
    process_banner: "susi-crewai",
    cli_name: "crewai",
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::python_engine;
    use std::path::PathBuf;

    #[test]
    fn example_mentions_crewai() {
        assert!(PROFILE.example_script.contains("from crewai import"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }

    #[test]
    fn init_writes_config() {
        let dir = std::env::temp_dir().join(format!(
            "susi-crewai-init-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = python_engine::init_workspace(&PROFILE, &dir).unwrap();
        assert!(PathBuf::from(report["config"].as_str().unwrap()).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
