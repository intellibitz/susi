# Extension pack manifest schema & 5-minute starter guide

Core ships and seeds no pack. Installed packs live at
`~/.susi/extensions/<id>/manifest.json` (written by the user or the
commercial desktop in Susi Commercial Apps, never by core). Core is only the
admission side: it validates, loads and unloads a pack on request.

## Required fields

| Field | Type | Notes |
|-------|------|--------|
| `id` | string | Stable pack id (directory name) |
| `name` | string | Human label |
| `version` | string | Pack semver |
| `apiVersion` | string | Major must match host (`"1"` today) |
| `capabilities` | string[] | Declared capability strings |
| `permissions` | string[] | Enforced (`filesystem.read`, `filesystem.write`, `network.egress`, `process.exec`); unknown strings rejected |

## Optional fields

| Field | Type | Notes |
|-------|------|--------|
| `description` | string | |
| `requires` | string[] | Other pack ids that must load first |
| `optional` | string[] | Soft dependencies |
| `files` | object | Map of logical name → path relative to pack root |
| `checksums` | object | Logical name → SHA-256 (raw hex or `sha256:` prefixed); verified on load |
| `signatures` | object | Logical name → Ed25519 signature (hex, 64 bytes) over the raw file bytes |
| `signer` | string | Ed25519 public key (hex, 32 bytes); required when `signatures` is set; pinned on first load |

## Rules

- Unknown JSON keys are preserved (Mandate 35).
- Paths in `files` must stay under the pack root unless `filesystem.read` is granted; absolute paths are always rejected.
- Optional pack validation failure must not abort daemon startup.

## Validate before you ship

`susi extensions validate <pack-dir>` applies the host's admission rules to a
pack directory anywhere on disk: manifest schema and API major, known
permissions, path containment, declared checksums and ed25519 signatures. It
reads and changes no host state, and prints the SHA-256 of every artifact the
host pins on `susi extensions load`. A non-zero exit means the host would
reject the pack.

---

## 5-minute starter: author, validate, and load your own pack

Third-party developers can package execution-agent adapters, external peer
agents, MCP server catalogs, or model catalog overrides into a self-contained
pack directory and load it into `susi` in four steps:

### 1. Scaffold the pack directory

```bash
# Create an empty pack shell under ~/.susi/extensions/my-starter-pack
susi extensions create my-starter-pack
```

Or create a directory anywhere on disk (for example `./my-starter-pack/`) while
developing your plug.

### 2. Add `execution-agents.json` and `external-peer-agents.json`

Create `my-starter-pack/execution-agents.json` to admit external coding agents
(`susi agents`):

```json
[
  {
    "id": "openhands-local",
    "peer_name": "OpenHandsLocalAgent",
    "name": "OpenHands Headless CLI"
  }
]
```

Create `my-starter-pack/external-peer-agents.json` to admit CLI or A2A peers:

```json
[
  {
    "name": "MyCustomCliAgent",
    "protocol": "cli",
    "detect_bins": ["my-agent-cli"],
    "args": ["{goal}"]
  }
]
```

### 3. Write `manifest.json` with SHA-256 checksums

Compute the SHA-256 digests of your files (`sha256sum execution-agents.json external-peer-agents.json`)
and bind them in `my-starter-pack/manifest.json`:

```json
{
  "id": "my-starter-pack",
  "name": "Community Starter Pack",
  "version": "0.1.0",
  "apiVersion": "1",
  "description": "Sample execution-agent and CLI peer pack for susi",
  "capabilities": [
    "catalog.execution_agents",
    "catalog.external_peers"
  ],
  "permissions": [
    "process.exec"
  ],
  "files": {
    "execution-agents.json": "execution-agents.json",
    "external-peer-agents.json": "external-peer-agents.json"
  },
  "checksums": {
    "execution-agents.json": "sha256:<YOUR_EXECUTION_AGENTS_SHA256>",
    "external-peer-agents.json": "sha256:<YOUR_EXTERNAL_PEERS_SHA256>"
  }
}
```

### 4. Validate and load the pack

```bash
# Dry-run admission check (verifies schema, permissions, path containment, and SHA-256 digests)
susi extensions validate ~/.susi/extensions/my-starter-pack

# Activate the pack on your host
susi extensions load my-starter-pack
susi extensions status
```

---

## Authoring a fuel-metered Wasm reflex (`reflex_<slug>`)

Deterministic Tier-0 reflexes are compiled to `wasm32-wasip1` and placed under
the host data directory's `reflexes/` folder (`~/.susi/reflexes/<slug>.wasm` or
`$XDG_DATA_HOME/susi/reflexes/<slug>.wasm`). The running host exposes each module
as the MCP tool `reflex_<slug>` inside its fuel-metered `Wasmi` sandbox (network
egress stripped by default).

1. Build a small Rust WASI binary targeting `wasm32-wasip1` that reads JSON
   arguments from `stdin` (or argv) and prints a deterministic JSON result to
   `stdout`.
2. Copy the compiled `<slug>.wasm` into your `reflexes/` directory via atomic
   rename (stage to `<slug>.wasm.tmp` then `mv` to `<slug>.wasm`).
3. Call it over MCP on port `9090` (`tools/call` with `"name": "reflex_<slug>"`).

See also `ARCHITECTURE.md` → Admission protocols and [docs/integrating-with-susi.md](integrating-with-susi.md).
