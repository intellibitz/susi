//! E2B sandbox runtime E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "e2b";
pub const CONFIG_ENV: &str = "SUSI_E2B_CONFIG";
pub const IMPORT_NAME: &str = "e2b";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install e2b e2b-code-interpreter",
    documentation: "https://e2b.dev/docs",
    scaffold_subdir: "e2b",
    example_script: r#"#!/usr/bin/env python3
"""Minimal E2B sandbox entry used by SUSI. Replace with your agent code runner."""
from __future__ import annotations

import json
import os
import sys

from e2b_code_interpreter import Sandbox


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    safe = prompt.replace("\\", "\\\\").replace('"', '\\"')
    code = f'print("e2b:", """{safe}""")'
    with Sandbox.create() as sandbox:
        result = sandbox.run_code(code)
        stdout = getattr(result, "text", None) or str(result)
    print(json.dumps({"prompt": prompt, "answer": stdout}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    credential_envs: &["E2B_API_KEY"],
    credential_hint: "export E2B_API_KEY=… (from https://e2b.dev)",
    process_banner: "susi-e2b",
    cli_name: "e2b",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_mentions_e2b() {
        assert!(PROFILE.example_script.contains("e2b"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }
}
