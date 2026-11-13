# Isolated cloud agent sessions

Cloud execution follows the same CEO mandates and canonical `AGENTS.md` as a
local Windows worker. A cloud session must not ask the CEO to inspect code,
perform setup steps or expose a personal computer to complete engineering work.

Each worker ALWAYS runs inside its own isolated linked worktree and branch (never
the primary checkout and never a shared worktree), with private build output
(`CARGO_TARGET_DIR`), unique `SUSI_AGENT` identity, reserved scopes and private
`SUSI_HOME`. There is no primary-checkout exemption for cloud containers or
`SUSI_CLOUD_SESSION`.

All local and cloud agents collaborate exclusively through the **Susi Native
Board** (`susi board check` / `susi board post`, HTTP gateway `/v1/board/*` via
`SUSI_BOARD_URL` + `SUSI_BOARD_TOKEN`, or shared git refs `refs/board/items/*`
via `SUSI_BOARD_REMOTE_URL` / Commercial `origin`). Unreachable or unconfigured
boards fail closed: no local/cloud agent may bypass board checks or mirror-free
writing.

Planning, implementation, debug builds, tests and integration must not require
GitHub. Repository hosting is optional transport. Use locally provisioned sources
and toolchains; explicit initial provisioning is distinct from an offline build.
Cargo offline resolution alone does not prevent dependency build scripts from
downloading binaries. Neither a container nor a warm cache establishes Rust-only
or license compliance.

The available native commands are `cargo local-workflow` and
`cargo local-build debug|production`. They are migration building blocks; consult
`docs/native-rust-direction.md` and actual test evidence for current capabilities.
Do not treat a missing local integration feature as permission to skip its gate
or force work through hosted CI. Agents implement the missing capability.

Tests use scratch homes, scrubbed credentials and isolated ports. Preserve the
production executable and the previous verified artifact. Production promotion
requires exact candidate evidence, policy compliance, health checks and rollback.
Windows qualification must run on native Windows/MSVC; a Linux cloud build is
not evidence that Windows works.

The former shell SessionStart hooks in `.claude/settings.json` and
`.codex/hooks.json` are removed. Tool-specific Markdown files point to AGENTS.md.
Legacy cloud-session scripts may remain for compatibility tests; they are not
required startup instructions or the governing workflow. Historical measurements
remain in `docs/workflow-speed.md` and do not override the native local contract.
