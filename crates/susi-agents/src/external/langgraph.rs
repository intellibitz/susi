//! LangGraph E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "langgraph";
pub const CONFIG_ENV: &str = "SUSI_LANGGRAPH_CONFIG";
pub const IMPORT_NAME: &str = "langgraph";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install langgraph",
    documentation: "https://langchain-ai.github.io/langgraph/",
    scaffold_subdir: "langgraph",
    example_script: r#"#!/usr/bin/env python3
"""Minimal LangGraph entry used by SUSI. Replace with your real graph."""
from __future__ import annotations

import json
import os
import sys

from langgraph.graph import END, START, StateGraph
from typing_extensions import TypedDict


class State(TypedDict):
    prompt: str
    answer: str


def respond(state: State) -> State:
    return {"prompt": state["prompt"], "answer": f"langgraph: {state['prompt']}"}


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    graph = StateGraph(State)
    graph.add_node("respond", respond)
    graph.add_edge(START, "respond")
    graph.add_edge("respond", END)
    app = graph.compile()
    result = app.invoke({"prompt": prompt, "answer": ""})
    print(json.dumps(result, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    example_filename: "graph.py",
    credential_envs: &[],
    credential_hint:
        "no API key required for local/stdlib entry (model keys stay in your graph script env)",
    process_banner: "susi-langgraph",
    cli_name: "langgraph",
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::python_engine;
    use std::path::PathBuf;

    #[test]
    fn example_mentions_langgraph() {
        assert!(PROFILE.example_script.contains("langgraph"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }

    #[test]
    fn init_writes_config() {
        let dir = std::env::temp_dir().join(format!(
            "susi-langgraph-init-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = python_engine::init_workspace(&PROFILE, &dir).unwrap();
        assert!(PathBuf::from(report["config"].as_str().unwrap()).is_file());
        assert!(PathBuf::from(report["script"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("graph"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
