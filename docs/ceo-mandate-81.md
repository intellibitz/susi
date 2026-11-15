> **Current Step 1 directive (2026-11-15, Mandate 80):** exactly three core pillars:
> **GAWD** (including A2A, ACP and Susi Harness parallel multi-agent workflow),
> **GEMI**, **GMCP**. Older architectural counts are superseded
> taxonomy, not removal of those capabilities. Review ready work for safe
> integration every ten minutes; synchronize source, tests, mandates and docs.
> Preserve ownership, release qualification, licensing and rollback.
> See `docs/ceo-mandate-80.md` (or `ceo-mandate-80.md` from this directory).

# CEO Mandate 81: Feature and Production Releases Must Sync the Local Host Binary and Live Daemon

Effective 2026-11-15. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 81**. It builds directly on **Mandate 48** (installed
Susi is the stable copy that tracks the latest qualified release), **Mandate 64**
(native Windows is first-class alongside Linux), and **Mandate 80** (three core
pillars and synchronized delivery). `intellibitz/susi-commercial-apps` carries the
paired commercial edition in its own `docs/ceo-mandate-81.md`.

**Agent reminder (one line):** Follow CEO Mandate 81 (`docs/ceo-mandate-81.md`):
every feature or production release cut or published for this host must sync
the local installed release binary (`~/.susi/bin/susi` / `susi.exe`) and
`susi.build.json`, restart and health-check the live daemon with rollback on
failure, keep Windows scheduled tasks at `RunLevel=Limited`, and enable the
unattended `susi-release-sync` timer on both Linux and Windows.

## 1. Release cuts sync the local host binary before Done

Cutting or publishing any qualified release (`vX.Y.Z`) whose release record
(`releases/vX.Y.Z.json`) covers the current host (`missing_targets` does not
list the host's target triple) is incomplete until the host's installed Susi
release matches `vX.Y.Z`:

1. `~/.susi/bin/susi` (`susi.exe` on Windows) reports `susi <X.Y.Z>`.
2. `~/.susi/bin/susi.build.json` records `"channel":"release"`,
   `"version":"<X.Y.Z>"`, and the tag's `"commit"`.
3. The previous installed binary is preserved at `~/.susi/bin/susi.prev`
   (`susi.prev.exe` on Windows).
4. If a local Susi daemon is running for that installation, it is cleanly
   restarted on the promoted executable and passes HTTP `/health` (reporting
   `"version": "<X.Y.Z>"`), rolling back automatically on health failure.

Dev builds (`target/debug/`, `target/fast-release/`, `cargo local-build`)
still never overwrite `~/.susi/bin/susi`; only `scripts/susi-release-sync.sh`
and the platform installers (`install.sh` / `install.ps1`) promote qualified
releases.

## 2. Verified post-qualify promotion (`--tag vX.Y.Z --built <path>`)

On the host where `release-qualify` just built the reference binary and
published `vX.Y.Z` + `releases/vX.Y.Z.json` to `origin`:

- `scripts/susi-release-sync.sh --tag vX.Y.Z --built <path>` verifies the
  remote tag commit against `releases/vX.Y.Z.json`, verifies that `<path>`'s
  SHA-256 equals the host asset's recorded `sha256` in `releases/vX.Y.Z.json`,
  verifies `<path> --version` reports `X.Y.Z`, and executes the full
  `wait_for_idle -> swap_in -> write_marker -> restart_service -> refresh_self`
  promotion pipeline (with rollback on health failure) without running a
  redundant second cold build from scratch.
- Any digest mismatch, missing reference record hash, version mismatch, or
  unqualified `main` binary passed to `--built` is hard-refused before touching
  `~/.susi/bin/susi`.
- All other hosts continue to build `vX.Y.Z` natively from the signed Git tag
  source checkout using the recorded toolchain, profile, features, and env.

## 3. Cross-OS unattended timer and Windows `RunLevel=Limited` invariant

- `scripts/susi-release-sync.sh --install-timer` / `--uninstall-timer` must
  succeed natively on both **Linux** (`systemd --user` `susi-release-sync.timer`
  / `susi-release-sync.service`) and **Windows** (Task Scheduler task
  `susi-release-sync`, matching `ServiceManagerKind::WindowsTask`).
- Every Susi Windows Scheduled Task (`susi-daemon`, `susi-release-sync`, and any
  auto-start task such as `SusiClusterAutoStart`) MUST use `LogonType=Interactive`
  with `RunLevel=Limited` (never `Highest`). Running a user daemon or sync task
  elevated breaks `Win32_Process.ExecutablePath` inspection, process-stop
  handles, and unattended promotion from standard shells (`Access Denied`).
- Both `install.ps1` and `susi-release-sync.sh` write `~/.susi/bin/susi.build.json`
  on Windows and Linux so `--check` and `susi-commercial workflow check` always
  report accurate channel/version/commit drift.

## 4. Session-start drift detection on Windows and Linux

- `susi-release-sync --check` reports `installed:`, `qualified:` (newest tag
  on `origin` whose `releases/<tag>.json` qualifies this host), and `latest:`.
- `susi-commercial workflow check` checks and launches `~/.susi/bin/susi-release-sync`
  in the background whenever `installed` is behind `qualified` — on Windows via
  `schtasks /Run /TN susi-release-sync` or Git `bash.exe` (avoiding direct
  execution of extensionless `#!` scripts and avoiding WSL `System32\bash.exe`),
  and on Linux via `systemctl --user start --no-block susi-release-sync.service`
  or direct script execution.

## 5. Implementation truth (2026-11-15)

- **Enforced in scripts, installers, workflow tools, and tests:**
  - `scripts/susi-release-sync.sh` implements `--tag vX.Y.Z --built <path>` with
    fail-closed SHA-256 verification against `releases/<tag>.json`, `qualified:`
    reporting in `--check`, and Windows Task Scheduler support in
    `--install-timer` / `--uninstall-timer`.
  - `scripts/susi-release-sync-windows.ps1` registers `susi-release-sync` with
    `RunLevel=Limited` and preserves fail-closed `Owned-Processes` identity checks.
  - `install.ps1` writes `susi.build.json`, copies updater scripts into
    `~/.susi/bin/`, and uses `-RunLevel Limited`.
  - `xtask release-qualify` prints the mandatory Step 6 host-sync command upon
    qualification completion.
  - `susi-commercial workflow check` (`crates/susi-dev-workflow`) resolves Git
    `bash.exe` / `schtasks` on Windows so release drift detection and background
    sync run on both Windows and Linux.
  - Regression tests in `tests/architecture_tests.rs`, `tests/installer_contract.rs`,
    `tests/release_process_identity.rs`, and `susi-commercial-apps` verify these
    contracts statically and with fixtures.
- **Not enforced:**
  - External scheduled tasks created manually outside Susi's installers with
    elevated privileges are refused fail-closed by `Owned-Processes` (never
    bypassed); operators or elevated one-shot repair scripts must lower their
    `RunLevel` to `Limited`.
  - `docs/releases.md` Step 6 still relies on the releasing agent executing the
    printed sync command after `git push origin vX.Y.Z` (or letting the hourly
    timer pick it up); pushing a tag over raw Git cannot synchronously rewrite a
    running Windows binary without the local promotion step.
