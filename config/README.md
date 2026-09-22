# Config layout

Bundled defaults under `config/` seed `~/.susi` on first run (Mandate 35:
host files win; unknown keys are preserved on write-back). Extension packs
under `config/extensions/<id>/` (and `~/.susi/extensions/<id>/`) carry vendor
opinions; see `ARCHITECTURE.md` plugin triad.

## Authority map

| File | Role |
|------|------|
| `config.default.json` | Runtime knobs, routing, `bootstrap_mcp_servers` (small auto-enable set), non-managed `external_peer_agents` (CLI/HTTP/A2A only), governance. Port keys (`gmcp_port` / `gemi_port` / …) may appear for documentation but are **not** heal-merged into host `config.json` — accessors always return `susi_paths::ports` (9090–9093). **Managed** peers are derived from `execution-agents.json` + `agent-engines.json` `peer_name` fields at load time. |
| `agents.default.json` | Native / DynamicAgent recruitment catalog (swarm ranks, anchors) |
| `leading-mcp.json` | Editorial top MCP ranks for `susi mcp` |
| `mcp.registry.default.json` | Broader scout corpus (many packages; not the same as bootstrap) |
| `coding-models.json` / `frontier-models.json` / `open-weight-models.json` / `openrouter-models.json` | Curated model catalog views |
| `models.catalog.default.json` | Progressive local weight ladder |
| `execution-agents.json` / `agent-engines.json` | External executor / framework adapter catalogs |
| `execution-peers.legacy.json` | Migration-only peer list |
| `prompts.default.json` / `messages.default.json` / `chat_templates.default.json` | Prompt / message templates |
| `extensions/default/*` | Default extension pack (`manifest.json`, `cloud-vendors.json`, catalog pointers) |

## Do not confuse

- **Bootstrap MCP** (`config.default.json` → `bootstrap_mcp_servers`) ≠ **leading MCP** ≠ **scout registry**. Different trust scores and sizes by design.
- **Catalog body** resolves via extension pack (`load_json_or_bundled`). **Per-id overrides** for coding/frontier/… still live under `~/.susi/<plane>/` (e.g. `coding-models/`). Edit pack for list membership; edit override dir for one model’s engine/key env.
- **Host-contract ports** are compile-time constants; polluted `config.json` port fields are ignored.
