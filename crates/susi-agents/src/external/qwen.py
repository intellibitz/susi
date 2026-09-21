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
