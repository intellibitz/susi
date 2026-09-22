//! Haystack (deepset) E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "haystack";
pub const CONFIG_ENV: &str = "SUSI_HAYSTACK_CONFIG";
pub const IMPORT_NAME: &str = "haystack";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install haystack-ai",
    documentation: "https://docs.haystack.deepset.ai/",
    scaffold_subdir: "haystack",
    example_script: r#"#!/usr/bin/env python3
"""Minimal Haystack pipeline entry used by SUSI. Replace with your document pipeline."""
from __future__ import annotations

import json
import os
import sys

from haystack import Pipeline
from haystack.components.builders import PromptBuilder
from haystack.components.generators import OpenAIGenerator


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    model = os.environ.get("HAYSTACK_MODEL", os.environ.get("OPENAI_MODEL", "gpt-4o-mini"))
    pipe = Pipeline()
    pipe.add_component("prompt", PromptBuilder(template="Answer concisely: {{query}}"))
    pipe.add_component("llm", OpenAIGenerator(model=model))
    pipe.connect("prompt.prompt", "llm.prompt")
    result = pipe.run({"prompt": {"query": prompt}})
    replies = result.get("llm", {}).get("replies") or []
    answer = replies[0] if replies else str(result)
    print(json.dumps({"prompt": prompt, "answer": str(answer)}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    credential_envs: &["OPENAI_API_KEY", "OPENROUTER_API_KEY"],
    credential_hint: "export OPENAI_API_KEY=… (or OPENROUTER_API_KEY + OPENAI_API_BASE)",
    process_banner: "susi-haystack",
    cli_name: "haystack",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_mentions_haystack() {
        assert!(PROFILE.example_script.contains("from haystack import"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }
}
