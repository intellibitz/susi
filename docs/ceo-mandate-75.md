# CEO Mandate 75: Raw Source (`main`) & Distill (`release`) Architecture, Cloud Raw-Source Feature Pickup, and Pure `Apache-2.0` License Compliance

Effective 2026-11-12. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 75**. It builds additively on Mandates 62–74
([`docs/ceo-agent-mandate.md`](ceo-agent-mandate.md) and
[`docs/ceo-mandate-74.md`](ceo-mandate-74.md)) and wins where older text differs.
`intellibitz/susi-commercial-apps` carries the paired commercial edition in its
own `docs/ceo-mandate-75.md` without weakening any commercial guardrail, license
authority rule (`SUSI_COMMERCIAL_SECRET`) or KeePassXC credential boundary.

The CEO sets the strategic vision, never inspects code, and delegates full
autonomous CTO execution to collaborating agents. Requirements stated here are
distinct from verified runtime status; section 5 records implementation truth vs.
ongoing work per Mandate 1.

**Agent reminder (one line):** Follow CEO Mandate 75 (`docs/ceo-mandate-75.md`):
operate both `susi` and `susi-commercial-apps` strictly on the **Raw Source
(`main`) -> Distill (`release`)** model with remote pull-and-merge reconciliation,
freely pick up capabilities from cloud raw sources (`local-harvest raw-features`,
`route`, `pickup`) into Susi Core's 5 `<10s` lean socket pillars or into closed,
licensed, paid `susi-commercial-apps`, enforce **Pure `Apache-2.0` ONLY** for all
cloud raw-source harvesting (full §4(a)–(d) compliance, hard-refusing MIT-only,
BSD-only, LLVM-exception, `AND`, copyleft and unlicensed code), and coordinate
additively across `Claude`, `Codex` and `Carbon` over portable Git storage and
the shared Susi Board.

## 1. Raw Source (`main` branch) & Distill (`release` branch) Philosophy

Both `intellibitz/susi` (free-forever Apache-2.0 open-core socket) and
`intellibitz/susi-commercial-apps` (closed, cryptographically licensed, paid
corporate suite) operate strictly on one symmetric delivery pipeline:
**Raw Source (`main`) -> Distill (`release`)**.

### 1.1 Raw Source (`main`)

- **Continuous, ungated raw feature stream:** `refs/heads/main` (the sole remote
  branch under `refs/heads/*` on `origin`) is the raw-source integration trunk.
  Raw features — both first-party agent-authored features and cloud-harvested
  open-source capabilities — land freely and continuously on `main` with zero
  mandatory pre-push gates, zero GitHub Actions, and zero pull requests.
- **Remote pull and merge at every step:** Every workflow lifecycle step
  (`start`, `sync` / `step`, `commit`, `distill`, `ship`, `release`, and parallel
  `Susi Harness` worker steps) reconciles with `origin` via `git fetch` plus
  fast-forward or `git merge-tree --write-tree` merge reconciliation when
  reachable (falling back cleanly to offline Git state when disconnected,
  Mandate 66). Every live publish also runs the provider-neutral storage guard
  (`local-snapshot guard`, recording witness refs under `refs/susi-guard/origin/`)
  to verify that `main` and candidate lines only moved forward and no tag moved
  or disappeared.
- **Structured provenance trailers (final commit paragraph, zero blank lines):**
  Every raw-source commit on `main` carries machine-readable Git trailers in its
  final paragraph with no blank lines separating trailers (`distill lint`
  requirement):
  - `Task: T-<AGENT>-<n>` (backed by `.agents/tasks/T-<AGENT>-<n>.json` declaring
    exact `scopes`, `deps`, and `accept.cmd`, closed when its work lands);
  - `Feature: <feature-id>` (and `Depends: <task-id>` when applicable);
  - When cloud-harvested: `Harvest: H-<AGENT>-<n>`, one `Origin: <url>@<sha>:<path>`
    trailer per imported mapping, and `Origin-License: Apache-2.0`.

### 1.2 Distill (`release`)

- **Deterministic Susi-owned distillation:** Susi — never GitHub or a web merge
  button — deterministically selects verified `Task:` / `Feature:` units and
  their transitive dependency closures from Raw `main` and composes conflict-free
  **Distill (`release`)** candidates (`distill`, `local-distill`, and
  `release-lanes`; see [`docs/distillation.md`](distillation.md),
  [`docs/release-distillation.md`](release-distillation.md), and
  [`docs/release-lanes.md`](release-lanes.md)).
- **Disjoint lanes and atomic Git coordination:** Parallel agents advance
  distillation without conflicts via disjoint file-scope lanes, atomic
  create-if-absent claim blobs (`refs/claims/*`), in-memory `git cherry-pick -x`
  / `git merge-tree --write-tree` composition with `git patch-id --stable`
  deduplication, and **one-version-one-cut** atomic version reservation
  (`release_version_guard` in `release-qualify`, `T-CLAUDEONECUT-1`) so two
  concurrent agents never qualify or cut the same release version.
- **Portable Git ref namespaces (`push-policy` enforced):**
  - `refs/heads/main`: sole remote `refs/heads/*` branch (fast-forward only,
    never deleted).
  - `refs/agents/<agent>`: non-branch agent handoff/inbox refs
    (`docs/agent-refs.md`), never triggering host pull-request banners.
  - `refs/candidates/release/<name>` and `refs/candidates/distill/<name>`:
    fast-forward-only candidate lines outside `refs/heads/*`.
  - `refs/claims/*`, `refs/claim-agents/*`, `refs/claim-generation/*`, and
    `refs/closed/*`: atomic blob leases, version reservations and close receipts.
  - `refs/tags/susi-distill/<name>` and `refs/tags/susi-snapshot/<name>`:
    immutable unqualified distillation/snapshot tags.
  - `refs/tags/vX.Y.Z`: immutable production release snapshots admitted **only**
    when exact-tree qualification passes all gates (including the pure
    `Apache-2.0` `harvest_audit`), bound to `releases/vX.Y.Z.json` and
    `releases/qualification/vX.Y.Z.json`.

## 2. Cloud Raw-Source Feature Pickup & Smart Routing (`Core` vs. `Commercial Apps`)

Susi does not reinvent capabilities that already exist cleanly in the open-source
ecosystem. Collaborating agents and `local-harvest` (`docs/harvest.md`) freely
discover, classify, and pick up raw features from public cloud Git repositories
(on any Git host — GitHub, GitLab, Codeberg, SourceHut, self-hosted) and raw
upstream/agent refs using pure `git fetch` quarantine without executing untrusted
upstream code (`local-harvest raw-features`, `local-harvest route`, and
`local-harvest pickup`).

Every candidate capability is deterministically routed between Susi's two
repositories (`local-harvest route` and automatic routing in
`local-harvest pickup`):

1. **Route to `core` (`intellibitz/susi`) ONLY for the 5 Lean Socket Pillars:**
   - A cloud raw-source capability enters `intellibitz/susi` **only** when it is
     a lean, zero-UI, zero-vendor-product protocol or runtime substrate primitive
     inside Susi Core's **5 primary open-core pillars**:
     - **GAWD** (`susi-gawd*` — evidence-gated swarm & A2A coordination)
     - **GEMI** (`susi-gemi*` — local & cloud model inference router)
     - **GMCP** (`susi-gmcp` — MCP protocol server & governed tool plane)
     - **ACP** (`susi-core::acp` — Agent Client Protocol / Agent Communication
       Protocol standard open socket)
     - **Susi Harness** (`susi-harness` — recursive driver/engine with built-in
       parallel multi-agent workflow)
   - Core pickups keep Susi Core minimal and blazing fast: warm
     `cargo fast-release` (`build_ms + test_ms + release_ms = total_ms < 10_000`,
     `< 10.0s` budget) and `tests/lean_core_ceilings.rs` ratchets must hold.
2. **Route to `commercial` (`intellibitz/susi-commercial-apps`) for All Add-Ons,
   Visual Interfaces & Enterprise Innovations:**
   - Every innovative open-source product feature, desktop or operator visual
     interface (all UIs and visual views live in Susi Commercial Apps),
     enterprise or department pack, external SaaS/MCP connector, SSO/RBAC/FinOps
     governance plane, multi-host fleet controller, and turnkey corporate
     workflow routes **exclusively** to `intellibitz/susi-commercial-apps`
     (closed-source, cryptographically licensed via `SUSI-ENT-v1` / Ed25519
     `SUSI-CUST-v1`, and paid corporate software).
   - `local-harvest pickup` hard-refuses importing `commercial`-routed or
     UI/add-on paths into `intellibitz/susi`, and code never flows from
     `susi-commercial-apps` into public core.

## 3. PURE `Apache-2.0` License ONLY for Cloud Harvest Compliance

For all cloud raw-source feature pickup and source harvesting across **both**
`intellibitz/susi` and `intellibitz/susi-commercial-apps`, **ONLY `Apache-2.0` is
permitted for pure compliance**:

- **Accepted (`Apache-2.0` exclusively):**
  - Files and repositories licensed under pure `Apache-2.0` pass as
    `"Apache-2.0"`.
  - Dual- or multi-licensed SPDX `OR` expressions offering unencumbered
    `Apache-2.0` (for example `Apache-2.0 OR MIT`, `MIT OR Apache-2.0`, or
    `(MIT OR Apache-2.0)`) pass **solely by electing and binding `"Apache-2.0"`
    exclusively** (`"license": "Apache-2.0"` in the harvest manifest, zero
    MIT/BSD fallback).
- **Hard-Refused (non-zero exit, zero files written):**
  - Standalone `MIT`-only, `BSD-2-Clause`-only, and `BSD-3-Clause`-only;
  - `Apache-2.0 WITH LLVM-exception` (unless pure `Apache-2.0` without exception
    is explicitly offered as an `OR` alternative);
  - Any `AND` combination requiring another license alongside `Apache-2.0`;
  - `ISC`, `MPL`, `EPL`, `CDDL`, `GPL`, `LGPL`, `AGPL`, `SSPL`, `BUSL`/`BSL`,
    Elastic, Commons Clause, source-available, non-commercial, custom, or
    unknown/unlicensed raw source.
  - Both Core's `local-harvest` (`scan`, `import`, `pickup`, `audit`) and
    Commercial Apps' `src/harvest_gate.rs` enforce this rule fail-closed.

### 3.1 Mandatory Apache-2.0 §4(a)–(d) & Provenance Truthfulness

Every harvested file across both repositories must satisfy all four obligations of
Apache License 2.0 Section 4 and pass `local-harvest audit`:

- **§4(a) License copy:** Include `LICENSES/Apache-2.0.txt` verbatim.
- **§4(b) Prominent change notice:** Every destination file modified from its
  pinned upstream Git blob (including any Rust port under `--mode port`) must
  carry an explicit `Modified by` notice (for example
  `// Modified by Susi contributors, 2026-11-12: <summary>`).
- **§4(c) Preserved notices:** Retain all upstream copyright, patent, trademark,
  and attribution notices in harvested source files; never appropriate upstream
  trademarks, product names, or logos (Apache-2.0 §6).
- **§4(d) Deterministic `THIRD_PARTY_NOTICES`:** Record `.agents/harvest/H-<AGENT>-<n>.json`
  (`susi-harvest-manifest-v1`, `"license": "Apache-2.0"`), regenerate
  `THIRD_PARTY_NOTICES` deterministically via `local-harvest notices`, and stamp
  `Harvest: H-<AGENT>-<n>`, `Origin: <url>@<sha>:<path>`, and
  `Origin-License: Apache-2.0` commit trailers.
- **Honest provenance separation (`T-CLAUDECOMMHARVEST-2`):** Only real external
  or cross-repository source copied/ported from a verified commit SHA in the
  cited source repository uses `susi-harvest-manifest-v1` and `Harvest:` /
  `Origin:` trailers. Original first-party IntelliBitz commercial plugs that
  merely implement Susi Core's public socket protocols must state truthful
  first-party authorship and must never cite non-existent or wrong-repo upstream
  commits.

### 3.2 Crate-Graph Closure (`deny.toml`, Mandate 68) vs. Raw-Source Pickup

- Existing `deny.toml` crate-graph verification for `Cargo.lock` transitive
  libraries retains Mandate 68's accurate permissive allowlist (`Apache-2.0`,
  `MIT`, `BSD-2-Clause`, `BSD-3-Clause`) so current locked workspace builds stay
  truthful.
- For new capabilities, **cloud raw-source pickup under pure `Apache-2.0` is
  strictly `Apache-2.0`-only and is preferred over adding new external crate
  dependencies**, advancing Mandate 69's lean-core dependency ratchet while
  keeping per-file license compliance uniform and machine-verifiable.

## 4. Multi-Agent Git & Susi Board Collaboration (`Claude`, `Codex`, `Carbon`)

Multiple autonomous frontier agents (`Claude`, `Codex`, `Antigravity` / `Carbon`,
`Grok`, and `Susi Harness`) build simultaneously on the ecosystem:

1. **Portable Git storage + shared Susi Board:** Agents coordinate through pure
   Git refs and `.agents/tasks/` handoff records on `origin` (zero GitHub
   Actions, zero hosted PRs/checks, zero `gh` CLI or GitHub API lock-in) plus the
   Susi Board (the native board of CEO Mandate 76, [`ceo-mandate-76.md`](ceo-mandate-76.md);
   the older file board in the workspace rules file was retired on 2026-11-12),
   with `.agents/tasks/` records on `origin/main` for an agent that cannot
   reach it.
2. **Pre-import harvest deconfliction:** Before importing an external
   `Apache-2.0` capability, agents verify the pinned commit with
   `local-harvest scan` / `route` and post the target repository URL, commit SHA,
   source paths, and capability on the Susi coordination board (or task record)
   so parallel agents never harvest the same upstream target twice.
3. **Additive preservation:** Always `git fetch origin --prune` and pull-merge
   latest `origin/main` before publishing, claim disjoint file scopes, keep all
   commit trailers in the final paragraph with zero blank lines, use
   `release_version_guard` / `git ls-remote origin` to respect active version
   reservations, and never force-push or overwrite peer commits.

## 5. Implementation Truth (2026-11-12)

- **Verified on `origin/main` (`b0ea8434b`, `9dec24f30`, `b77701ca4`, Commercial
  `5bc32ef`):** `local-harvest` (`scan`, `import`, `audit`) and distillation
  `harvest_audit` enforce pure `Apache-2.0`-only admission (`T-CLAUDEHARVESTAPACHE-1`);
  `release_version_guard` enforces atomic one-version-one-cut reservations on
  `origin` (`T-CLAUDEONECUT-1`); provider-neutral `push-policy` and post-publish
  `local-snapshot guard` witness `main`, candidates, and immutable tags across
  both repositories (`T-CLAUDESTORAGEGUARD-1..5`, `T-CLAUDECOMMSTORAGEGUARD-1..2`).
- **Wave 2 CTO deliverables (`F-CTO-MANDATE-75`):** Mandate 75 constitutional
  codification (`docs/ceo-mandate-75.md`, `.agents/identity.json` Mandate 75, and
  synchronized front-door governance); `local-harvest raw-features`, `route`, and
  one-step `pickup` with Core 5-pillar vs. Commercial routing enforcement and
  `tests/pure_apache2_cloud_harvest.rs`; Commercial `src/harvest_gate.rs`
  pure-`Apache-2.0` defense-in-depth plus `T-CLAUDECOMMHARVEST-2` truthful
  provenance and the first verified external `Apache-2.0` open-source harvest.
- **Still open beyond Wave 2:** Windows x86_64 host execution of
  `T-CLAUDESTORAGEGUARD-3` commercial verification, multi-tree speculative
  qualification cache reuse, secondary-mirror portability drills, and multi-run
  `Susi Harness` benchmark parity for default-driver promotion.
