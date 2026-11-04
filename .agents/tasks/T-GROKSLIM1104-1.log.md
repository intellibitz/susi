# T-GROKSLIM1104-1

Signed: GROK

Branch: grok/slimcore-20261104
Worktree: F:\src\github.com\intellibitz\susi-grok-slimcore-20261104
Base: cf475e86f (origin/main at task start)

## Status

Measured, and the local checks passed. The push did not happen. `susi tasks claim` refuses because other live claims already reserve paths this branch edits, and the pre-push guard refuses a task with no claim. No pull request was opened.

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

## After (default release, this branch)

Binary: `target\release\susi.exe` copied to `%LOCALAPPDATA%\Temp\susi-slim-T-GROKSLIM1104-1.exe`. Built with `cargo build --release --locked --offline -p susi` after the feature gate and the merge of origin/main. The baseline copy was not overwritten.

- Version: 0.21.50 (the version moved with origin/main; this change does not bump it)
- Size: 44,305,920 bytes (42.25 MiB)
- SHA256: `9D56A4FC817A4F1280ABB4A113F04C676F385BBA0B07B697CCCB7E89C029A062`
- Delta vs baseline: −10,728,960 bytes (−10.23 MiB, −19.5%)
- `susi --version`: 101 ms, then 29 ms, then 26 ms. The baseline's first 2787 ms included a cold disk read of the old binary. These three runs were after that file had already been hashed.
- Time to ready (`susi start` until the host contract returns): 1431 ms cold, 735 ms warm (stopped, then started again on the same home)
- A second `susi start` while that daemon was already up returned in 52 ms
- Stop: 1591 ms, then 363 ms (already stopped)
- Isolated home: `%LOCALAPPDATA%\Temp\susi-slim-home-T-GROKSLIM1104-1`
- `SUSI_PORT_OFFSET=42100` so the ports were 51190 (MCP), 51191 (GEMI), 51192 (UDP discovery), 51193 (MCP alias), 51194 (A2A)
- The daemon was stopped after the probes

Protocol smoke on that running daemon (no token printed):

- MCP `POST /mcp` on 51190 and on 51193, with `Accept: application/json, text/event-stream`: 200 `text/event-stream`
- OpenAI-style `POST /v1/chat/completions` on 51191, with the host bearer: 400 `invalid_request_error` (the route is live; the probe sent no real model)
- Anthropic `POST /v1/messages` on 51191, with `x-api-key`: 400 `invalid_request_error`
- `GET /health` on 51191: 200
- A2A `GET /.well-known/agent-card.json` on 51194: 200
- Unauthenticated HTTP on those ports: 401
- A UDP datagram to 51192 was accepted by the socket

## Still linked (later passes)

- Vendor encyclopedia `susi-vendor-models`, including the compiled frontier, open-weight, and OpenRouter lists
- Vendor CLI wrappers `susi-vendor-cloud` and `susi-vendor-agents`
- Wasmtime, which is the plug-in host, not a third-party model implementation
- Slim GGUF admission is a magic-byte check (header `GGUF` and at least 64 bytes), not a tensor-table parse

## Finished

- Task reserved as T-GROKSLIM1104-1 on the dedicated worktree. No other worktree was edited.
- Baseline size and time-to-ready recorded above.
- Default `cargo check -p susi --offline --locked --tests` succeeded without Candle.
- `cargo test --offline --locked -p susi-gemi -p susi-gemi-models --lib slim_core` passed again after the origin/main merge (4 tests).
- `cargo fmt --all --check` passed.
- `cargo deny --offline --locked check licenses` passed.
- `cargo check -p susi --features plugins --offline --locked` passed. That build links Candle again.
- `cargo clippy --workspace --all-targets --offline --locked -- -D warnings` passed.

## Push blocked

`git push` was refused before any ref was updated: commit `3fdce51b` names T-GROKSLIM1104-1, and nobody holds that claim. A directory-wide claim overlaps `T-DSP4WIRE1104-9` (`tests/socket_parity.rs` and `tests/architecture_tests.rs`). Claiming only the edited files still overlaps these live claims:

- `crates/susi-daemon/src/ambient.rs` — T-DAEMON5TA1104-1 (about 470 min left)
- `crates/susi-gemi/src/engines/runtime_native.rs` — T-GEMI5TE1104-1 (about 508 min left)
- `crates/susi-gemi-models/src/lib.rs`, `lifecycle/provision.rs`, `lifecycle/selection.rs` — T-GEMIMODELS5TA1104-1 (about 84 min left)
- `tests/distill_loop_tests.rs` — T-ROOTTESTS5TB1104-1 (about 212 min left)

Those edits are the feature gate. They were not reverted, and the other claims were not released.

## Left

- `susi-vendor-syn` is still in the default graph through `susi-gawd`, not through `tools-rich`.
- Vendor encyclopedia `susi-vendor-models` (frontier, open-weight, and OpenRouter lists).
- Vendor CLI wrappers `susi-vendor-cloud` and `susi-vendor-agents`.
- Wasmtime stays; it is the plug-in host.
- A full `--features plugins` release rebuild was not repeated. `cargo tree -p susi --features plugins` is the proof those crates link again. It would replace the measured `susi.exe`.

Signed: GROK
