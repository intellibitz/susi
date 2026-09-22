//! AutoGen (AgentChat) E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "autogen";
pub const CONFIG_ENV: &str = "SUSI_AUTOGEN_CONFIG";
pub const IMPORT_NAME: &str = "autogen_agentchat";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install \"autogen-agentchat\" \"autogen-ext[openai]\"",
    documentation: "https://microsoft.github.io/autogen/",
    scaffold_subdir: "autogen",
    example_script: r#"#!/usr/bin/env python3
"""Minimal AutoGen AgentChat entry used by SUSI. Replace with your real agents."""
from __future__ import annotations

import asyncio
import json
import os
import sys

from autogen_agentchat.agents import AssistantAgent
from autogen_ext.models.openai import OpenAIChatCompletionClient


async def _run(prompt: str) -> str:
    model = os.environ.get("AUTOGEN_MODEL", os.environ.get("OPENAI_MODEL", "gpt-4o-mini"))
    model_client = OpenAIChatCompletionClient(model=model)
    agent = AssistantAgent(
        "susi_autogen",
        model_client=model_client,
        system_message="You are a helpful coding assistant invoked by SUSI. Be concise.",
    )
    try:
        result = await agent.run(task=prompt)
        texts = []
        for msg in getattr(result, "messages", []) or []:
            content = getattr(msg, "content", None)
            if content is not None:
                texts.append(str(content))
            else:
                texts.append(str(msg))
        return texts[-1] if texts else str(result)
    finally:
        await model_client.close()


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    answer = asyncio.run(_run(prompt))
    print(json.dumps({"prompt": prompt, "answer": answer}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    example_filename: "agent.py",
    credential_envs: &[
        "OPENAI_API_KEY",
        "AZURE_OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
    ],
    credential_hint:
        "export OPENAI_API_KEY=… (or AZURE_OPENAI_API_KEY / OPENROUTER_API_KEY + base URL)",
    process_banner: "susi-autogen",
    cli_name: "autogen",
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::python_engine;
    use std::path::PathBuf;

    #[test]
    fn example_mentions_autogen() {
        assert!(PROFILE
            .example_script
            .contains("from autogen_agentchat.agents import"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }

    #[test]
    fn init_writes_config() {
        let dir = std::env::temp_dir().join(format!(
            "susi-autogen-init-{}",
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
