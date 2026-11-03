# Extension pack manifest schema

Bundled example: [`config/extensions/default/manifest.json`](../config/extensions/default/manifest.json).  
Host overrides: `~/.susi/extensions/<id>/manifest.json`.

## Required fields

| Field | Type | Notes |
|-------|------|--------|
| `id` | string | Stable pack id (directory name) |
| `name` | string | Human label |
| `version` | string | Pack semver |
| `apiVersion` | string | Major must match host (`"1"` today) |
| `capabilities` | string[] | Declared capability strings |
| `permissions` | string[] | Enforced; unknown strings rejected |

## Optional fields

| Field | Type | Notes |
|-------|------|--------|
| `description` | string | |
| `requires` | string[] | Other pack ids that must load first |
| `optional` | string[] | Soft dependencies |
| `files` | object | Map of logical name → path relative to pack root |

## Rules

- Unknown JSON keys are preserved (Mandate 35).
- Paths in `files` must stay under the pack root unless `filesystem.read` is granted; absolute paths are always rejected.
- Optional pack validation failure must not abort daemon seed.

## Validate before you ship

`susi extensions validate <pack-dir>` applies the host's admission rules to a
pack directory anywhere on disk: manifest schema and API major, known
permissions, path containment, declared checksums and ed25519 signatures. It
reads and changes no host state, and prints the SHA-256 of every artifact the
host pins on `susi extensions load`. A non-zero exit means the host would
reject the pack.

See also `ARCHITECTURE.md` → Plugin triad.
