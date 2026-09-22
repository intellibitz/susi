//! Hugging Face smolagents E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "smolagents";
pub const CONFIG_ENV: &str = "SUSI_SMOLAGENTS_CONFIG";
pub const IMPORT_NAME: &str = "smolagents";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install smolagents",
    documentation: "https://huggingface.co/docs/smolagents",
    scaffold_subdir: "smolagents",
    example_script: r#"#!/usr/bin/env python3
"""Minimal Hugging Face smolagents entry used by SUSI. Replace with your real agent."""
from __future__ import annotations

import json
import os
import sys

from smolagents import CodeAgent


def _model():
    model_id = os.environ.get(
        "SMOLAGENTS_MODEL",
        os.environ.get("OPENAI_MODEL", "gpt-4o-mini"),
    )
    if os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENROUTER_API_KEY"):
        from smolagents import OpenAIServerModel

        kwargs = {"model_id": model_id}
        if base := os.environ.get("OPENAI_BASE_URL"):
            kwargs["api_base"] = base
        if key := os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENROUTER_API_KEY"):
            kwargs["api_key"] = key
        return OpenAIServerModel(**kwargs)
    from smolagents import InferenceClientModel

    return InferenceClientModel(
        model_id=os.environ.get(
            "SMOLAGENTS_MODEL",
            "Qwen/Qwen2.5-Coder-32B-Instruct",
        )
    )


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    agent = CodeAgent(tools=[], model=_model(), add_base_tools=False)
    output = agent.run(prompt)
    print(json.dumps({"prompt": prompt, "answer": str(output)}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    example_filename: "agent.py",
    credential_envs: &[
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "HF_TOKEN",
        "HUGGINGFACEHUB_API_TOKEN",
        "ANTHROPIC_API_KEY",
    ],
    credential_hint:
        "export OPENAI_API_KEY=… (OpenAI-compatible) or HF_TOKEN (InferenceClientModel)",
    process_banner: "susi-smolagents",
    cli_name: "smolagents",
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::python_engine;
    use std::path::PathBuf;

    #[test]
    fn example_mentions_smolagents() {
        assert!(PROFILE.example_script.contains("from smolagents import"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }

    #[test]
    fn init_writes_config() {
        let dir = std::env::temp_dir().join(format!(
            "susi-smolagents-init-{}",
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
