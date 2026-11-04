# T-GROKSLIM1104-1

Signed: GROK

Branch: grok/slimcore-20261104
Worktree: F:\src\github.com\intellibitz\susi-grok-slimcore-20261104
Base: cf475e86f (origin/main at task start)

## Status

In progress. Baseline is measured. The default build keeps `local-engine` and `tools-rich` off. `cargo check` of the default graph succeeded, and `cargo tree -p susi` no longer contains `susi-vendor-candle`. Slim-core unit tests passed. The slim release binary, startup measurement, sync with origin/main, and the pull request are still open.

## Baseline (default release, before this change)

Binary: `target\release\susi.exe` copied to `%LOCALAPPDATA%\Temp\susi-baseline-T-GROKSLIM1104-1.exe`.

- Version: 0.21.49
- Size: 55,034,880 bytes (52.49 MiB)
- SHA256: `168E982893464C457CDAE02A8F4FE8AE14659B9EE21EAE604CF704A9AD150C84`
- `susi --version`: 2787 ms, then 54 ms, then 62 ms
- Time to ready (`susi start` until the host contract returns): 1665 ms cold, 731 ms warm
- Stop: 2461 ms, then 2549 ms
- Isolated home: `%LOCALAPPDATA%\Temp\susi-baseline-home-T-GROKSLIM1104-1`
- `SUSI_PORT_OFFSET=41100` so the ports were 50190 (MCP), 50191 (GEMI), 50192 (UDP discovery), 50193 (MCP alias), 50194 (A2A)
- The daemon was stopped after both runs

## This slice

Off by default:

- `tools-rich`: browser, tantivy, fastembed, and syn AST tools
- `local-engine`: the Candle GGUF engine, tokenizer parsing, and alpha training

Turn both back on with `cargo build -p susi --features plugins`. MCP, OpenAI-style `/v1/chat/completions`, Anthropic `/v1/messages`, and A2A stay in the default binary. A missing local engine returns a typed inference error. It does not invent text.

## Still linked (later passes)

- Vendor encyclopedia `susi-vendor-models`, including the compiled frontier, open-weight, and OpenRouter lists
- Vendor CLI wrappers `susi-vendor-cloud` and `susi-vendor-agents`
- Wasmtime, which is the plug-in host, not a third-party model implementation
- Slim GGUF admission is a magic-byte check (header `GGUF` and at least 64 bytes), not a tensor-table parse

## Finished

- Task reserved as T-GROKSLIM1104-1 on the dedicated worktree. No other worktree was edited.
- Baseline size and time-to-ready recorded above.
- Default `cargo check -p susi --offline --locked --tests` succeeded without Candle.
- `cargo test --offline --locked -p susi-gemi -p susi-gemi-models --lib slim_core` passed (4 tests).

## Left

- Measure the slim release binary the same way as the baseline.
- Confirm `--features plugins` puts Candle and the rich tools back on the graph.
- Run clippy on the changed crates and `cargo deny`.
- Sync with origin/main, push, and open a pull request. Do not merge.

Signed: GROK
