//! OpenAI Agents SDK E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "openai-agents";
pub const CONFIG_ENV: &str = "SUSI_OPENAI_AGENTS_CONFIG";
pub const IMPORT_NAME: &str = "agents";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install openai-agents",
    documentation: "https://openai.github.io/openai-agents-python/",
    scaffold_subdir: "openai-agents",
    example_script: r#"#!/usr/bin/env python3
"""Minimal OpenAI Agents SDK entry used by SUSI. Replace with your real agent."""
from __future__ import annotations

import json
import os
import sys

from agents import Agent, Runner


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    agent = Agent(
        name="SusiOpenAIAgents",
        instructions="You are a helpful coding assistant invoked by SUSI. Be concise.",
    )
    result = Runner.run_sync(agent, prompt)
    output = getattr(result, "final_output", None) or str(result)
    print(json.dumps({"prompt": prompt, "answer": output}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    example_filename: "agent.py",
    credential_envs: &["OPENAI_API_KEY", "OPENROUTER_API_KEY"],
    credential_hint: "export OPENAI_API_KEY=… (or OPENROUTER_API_KEY + OPENAI_BASE_URL=https://openrouter.ai/api/v1)",
    process_banner: "susi-openai-agents",
    cli_name: "openai-agents",
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::python_engine;
    use std::path::PathBuf;

    #[test]
    fn example_mentions_agents() {
        assert!(PROFILE.example_script.contains("from agents import"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }

    #[test]
    fn init_writes_config() {
        let dir = std::env::temp_dir().join(format!(
            "susi-openai-agents-init-{}",
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
