//! n8n automation trigger E2E management via shared Python engine helpers.

use super::python_engine::EngineProfile;

pub const ENGINE_ID: &str = "n8n";
pub const CONFIG_ENV: &str = "SUSI_N8N_CONFIG";
/// Stdlib-only entry; doctor still verifies Python + config + webhook/API credentials.
pub const IMPORT_NAME: &str = "urllib";

pub const PROFILE: EngineProfile = EngineProfile {
    engine_id: ENGINE_ID,
    config_env: CONFIG_ENV,
    import_name: IMPORT_NAME,
    package_hint: "install n8n (npm i -g n8n) or use a hosted instance; script uses stdlib urllib",
    documentation: "https://docs.n8n.io/",
    scaffold_subdir: "n8n",
    example_script: r#"#!/usr/bin/env python3
"""Minimal n8n webhook/API trigger used by SUSI. Point N8N_WEBHOOK_URL at your workflow."""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    url = os.environ.get("N8N_WEBHOOK_URL", "").strip()
    if not url:
        raise SystemExit(
            "set N8N_WEBHOOK_URL to an n8n webhook (or production URL) that accepts JSON {\"prompt\":…}"
        )
    body = json.dumps({"prompt": prompt, "source": "susi"}).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    api_key = os.environ.get("N8N_API_KEY", "").strip()
    if api_key:
        req.add_header("X-N8N-API-KEY", api_key)
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", errors="replace")
        raise SystemExit(f"n8n webhook failed HTTP {e.code}: {raw}") from e
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        parsed = {"raw": raw}
    print(json.dumps({"prompt": prompt, "answer": parsed}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#,
    example_filename: "agent.py",
    credential_envs: &["N8N_WEBHOOK_URL", "N8N_API_KEY"],
    credential_hint: "export N8N_WEBHOOK_URL=https://…/webhook/… (optional N8N_API_KEY for REST)",
    process_banner: "susi-n8n",
    cli_name: "n8n",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_mentions_webhook() {
        assert!(PROFILE.example_script.contains("N8N_WEBHOOK_URL"));
        assert!(PROFILE.example_script.contains("SUSI_AGENT_PROMPT"));
    }
}
