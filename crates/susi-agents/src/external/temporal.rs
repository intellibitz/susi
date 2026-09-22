//! Temporal durable-workflow E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "temporal";
pub const CONFIG_ENV: &str = "SUSI_TEMPORAL_CONFIG";
pub const IMPORT_NAME: &str = "temporalio";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "pip/uv install temporalio",
    documentation: "https://docs.temporal.io/develop/python",
    scaffold_subdir: "temporal",
    example_script: r#"#!/usr/bin/env python3
"""Minimal Temporal client entry used by SUSI (echo workflow via local stub).

For production, point TEMPORAL_ADDRESS at your cluster and register a real worker.
This scaffold demonstrates durable-client wiring without requiring a live cluster
when TEMPORAL_DRY_RUN=1 (default).
"""
from __future__ import annotations

import asyncio
import json
import os
import sys


async def _run(prompt: str) -> str:
    if os.environ.get("TEMPORAL_DRY_RUN", "1") not in ("0", "false", "False"):
        return f"temporal-dry-run: {prompt}"
    from temporalio.client import Client

    address = os.environ.get("TEMPORAL_ADDRESS", "localhost:7233")
    namespace = os.environ.get("TEMPORAL_NAMESPACE", "default")
    client = await Client.connect(address, namespace=namespace)
    handle = await client.start_workflow(
        "SusiEchoWorkflow",
        prompt,
        id=f"susi-{abs(hash(prompt)) % 10_000_000}",
        task_queue=os.environ.get("TEMPORAL_TASK_QUEUE", "susi-task-queue"),
    )
    return str(await handle.result())


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    answer = asyncio.run(_run(prompt))
    print(json.dumps({"prompt": prompt, "answer": answer}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    example_filename: "agent.py",
    credential_envs: &[],
    credential_hint:
        "optional: TEMPORAL_ADDRESS / TEMPORAL_NAMESPACE (default dry-run without cluster)",
    process_banner: "susi-temporal",
    cli_name: "temporal",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_mentions_temporalio() {
        assert!(PROFILE.example_script.contains("temporalio"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }
}
