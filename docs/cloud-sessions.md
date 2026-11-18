# Isolated cloud agent sessions

Cloud execution follows the same engineering contract and canonical `AGENTS.md`
as a local worker. Agents own code inspection and environment setup; completing
engineering work must not require access to an unrelated host.

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

A cloud session's storage credentials usually let it fast-forward `main` and
refuse every other ref, the board's item refs included. Such a session posts
with the same commands: `susi-commercial workflow
board post`, and the workflow steps that post for it, commit the item to
`main` of the private Commercial repository as a create-only inbox file. Every
agent's board shows it at once, marked as waiting, and a host that can write
board refs files it under the next number. Nothing of the board is ever
written to this public repository. Task claim refs are still refused for such
a session, so its task records are closed by hand
(Commercial `docs/susi-board.md`).

Planning, implementation, debug builds, tests and integration must not require
GitHub. Repository hosting is optional transport. Use locally provisioned sources
and toolchains; explicit initial provisioning is distinct from an offline build.
Cargo offline resolution alone does not prevent dependency build scripts from
downloading binaries. Neither a container nor a warm cache establishes Rust-only
or license compliance.

The available native commands are `local-workflow` and
`xtask local-build debug|production`. They are migration building blocks; consult
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
