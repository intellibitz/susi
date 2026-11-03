# Config layout

Bundled defaults under `config/` seed `~/.susi` on first run (Mandate 35:
host files win; unknown keys are preserved on write-back). Core bundles no
launchable vendor catalog and seeds no extension pack: packs installed under
`~/.susi/extensions/<id>/` (by the user or the commercial desktop) carry
vendor opinions such as MCP servers and agent adapters; see `ARCHITECTURE.md`
plugin triad.

## Authority map

| File | Role |
|------|------|
| `config.default.json` | Runtime knobs, routing, `bootstrap_mcp_servers` and `external_peer_agents` (both empty in core; host `config.json` may list its own), governance. Port keys (`gmcp_port` / `gemi_port` / …) may appear for documentation but are **not** heal-merged into host `config.json` — accessors always return `susi_paths::ports` (9090–9094). **Managed** peers are derived from the active pack's `execution-agents.json` + `agent-engines.json` `peer_name` fields and from adapter files under `~/.susi/execution-agents/` / `~/.susi/agent-engines/` at load time. |
| `agents.default.json` | Native / DynamicAgent recruitment catalog (swarm ranks, anchors) |
| `cloud-vendors.json` | Cloud vendor key-env aliases the key resolver reads (compiled default; a pack may override) |
| `coding-models.json` / `frontier-models.json` / `open-weight-models.json` / `openrouter-models.json` | Curated model catalog views |
| `models.catalog.default.json` | Progressive local weight ladder |
| `execution-peers.legacy.json` | Migration-only peer list |
| `prompts.default.json` / `messages.default.json` / `chat_templates.default.json` | Prompt / message templates |

Launchable catalogs (`leading-mcp.json`, `mcp-registry.json`,
`execution-agents.json`, `agent-engines.json`) are not in core: they ship in
extension packs (manifest schema: [`docs/extension-pack.md`](../docs/extension-pack.md)).

## Do not confuse

- **Bootstrap MCP** (`config.default.json` → `bootstrap_mcp_servers`) ≠ **leading MCP** ≠ **scout registry**. Different trust scores and sizes by design. Core never enables or launches any of them on its own: a server runs only after an explicit `susi mcp-add` / `susi mcp enable` (an upgraded host keeps servers an earlier core enabled until `susi mcp disable <id>` removes them; `susi mcp list` shows them).
- **Catalog body** resolves via extension pack (`load_json_or_bundled`). **Per-id overrides** for coding/frontier/… still live under `~/.susi/<plane>/` (e.g. `coding-models/`). Edit pack for list membership; edit override dir for one model’s engine/key env.
- **Host-contract ports** are compile-time constants; polluted `config.json` port fields are ignored.
- **`auto_download_models`** is deprecated and has no effect: core never downloads a model by itself (CEO decision 2026-11-04). The key stays, defaulting to `false`, so existing `config.json` files still load; a model downloads only on an explicit request such as `susi os manage model install starter`.
