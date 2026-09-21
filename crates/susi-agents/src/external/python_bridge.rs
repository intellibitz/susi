//! Host-side bridge scripts for optional external Python adapters.
//!
//! These strings are the only Python payloads shipped by susi; they are not
//! first-class source files. Operator packages and credentials stay outside
//! the repo. Application source remains Rust.

/// Bundled Qwen-Agent runner; configuration and authentication stay with the user.
pub const QWEN_BRIDGE: &str = r#"
"""Bundled Qwen-Agent runner; configuration and authentication stay with the user."""
import json
import os
from pathlib import Path
from qwen_agent.agents import Assistant

config_path = Path(os.environ['SUSI_QWEN_CONFIG'])
if not config_path.is_absolute():
    raise ValueError('SUSI_QWEN_CONFIG must be an absolute path')
config = json.loads(config_path.read_text())
# No guessed model or provider. Tools are explicitly configured by the operator.
if not config.get('llm', {}).get('model'):
    raise ValueError('Qwen configuration requires llm.model')
if not config.get('function_list'):
    raise ValueError('Qwen configuration requires function_list for task execution')
agent = Assistant(**config)
for messages in agent.run(messages=[{'role': 'user', 'content': os.environ['SUSI_AGENT_PROMPT']}]):
    print(json.dumps(messages, ensure_ascii=False), flush=True)
"#;

/// Bundled agent-framework runner. Entry config and credentials stay with the operator.
pub const FRAMEWORK_BRIDGE: &str = r#"
"""Bundled agent-framework runner. Entry config and credentials stay with the operator."""
import importlib
import json
import os
import runpy
import sys
from pathlib import Path

config_path = Path(os.environ["SUSI_FRAMEWORK_CONFIG"])
if not config_path.is_absolute():
    raise ValueError("SUSI_FRAMEWORK_CONFIG must be an absolute path")
config = json.loads(config_path.read_text())
prompt = os.environ["SUSI_AGENT_PROMPT"]

if script := config.get("script"):
    script_path = Path(script)
    if not script_path.is_absolute():
        raise ValueError("config.script must be an absolute path")
    # Script reads SUSI_AGENT_PROMPT; do not reinterpret prompt as code.
    runpy.run_path(str(script_path), run_name="__main__")
elif entry := config.get("entrypoint"):
    if ":" not in entry:
        raise ValueError("config.entrypoint must be module:callable")
    module_name, _, attr = entry.partition(":")
    if not module_name or not attr or "/" in module_name or "\\" in module_name:
        raise ValueError("config.entrypoint must be a dotted module:callable")
    target = importlib.import_module(module_name)
    for part in attr.split("."):
        target = getattr(target, part)
    result = target(prompt)
    if result is not None:
        print(result, flush=True)
else:
    raise ValueError(
        "framework config requires absolute script or module:callable entrypoint"
    )
sys.stdout.flush()
"#;
