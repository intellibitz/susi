//! LlamaIndex E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "llamaindex";
pub const CONFIG_ENV: &str = "SUSI_LLAMAINDEX_CONFIG";
pub const IMPORT_NAME: &str = "llama_index";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install llama-index",
    documentation: "https://docs.llamaindex.ai/en/stable/understanding/agent/",
    scaffold_subdir: "llamaindex",
    example_script: r#"#!/usr/bin/env python3
"""Minimal LlamaIndex agent entry used by SUSI. Replace with your RAG/agent graph."""
from __future__ import annotations

import json
import os
import sys

from llama_index.core.agent import ReActAgent
from llama_index.core.tools import FunctionTool
from llama_index.llms.openai import OpenAI


def echo(text: str) -> str:
    """Return the input text unchanged."""
    return text


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    model = os.environ.get("LLAMAINDEX_MODEL", os.environ.get("OPENAI_MODEL", "gpt-4o-mini"))
    llm = OpenAI(model=model)
    tool = FunctionTool.from_defaults(fn=echo)
    agent = ReActAgent.from_tools([tool], llm=llm, verbose=False)
    response = agent.chat(prompt)
    print(
        json.dumps({"prompt": prompt, "answer": str(response)}, ensure_ascii=False),
        flush=True,
    )


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    credential_envs: &["OPENAI_API_KEY", "OPENROUTER_API_KEY"],
    credential_hint: "export OPENAI_API_KEY=… (OpenRouter: OPENROUTER_API_KEY + OPENAI_BASE_URL)",
    process_banner: "susi-llamaindex",
    cli_name: "llamaindex",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_mentions_llamaindex() {
        assert!(PROFILE.example_script.contains("llama_index"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }
}
