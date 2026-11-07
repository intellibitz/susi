---
name: Bug Report
about: Report a reproducible bug in the susi daemon, CLI, or host contract (ports 9090–9094)
title: "[Bug]: "
labels: ["bug"]
assignees: []
---

## Summary
A concise description of the bug and which component or port (`9090` MCP, `9091` OpenAI/Anthropic HTTP, `9092/9094` A2A, CLI, or crate) is affected.

## Environment
- **`susi --version`**:
- **OS & Architecture** (e.g., Ubuntu 24.04 `x86_64`, Windows 11 `x86_64`, macOS `aarch64` portable source):
- **Local / Cloud Engines Configured** (e.g., Ollama, vLLM, GGUF, OpenAI, Anthropic):

## Steps to Reproduce
1. Run `SUSI_HOME=/tmp/susi-repro SUSI_PORT_OFFSET=700 susi start`
2. Execute `...`
3. Observe error:

## Expected Behavior
What should have happened according to the host contract or CLI specification.

## Actual Behavior & Logs
Paste relevant output or redacted trace snippets (never paste live API keys or `~/.susi/api_token`).
