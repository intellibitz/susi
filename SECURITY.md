# Security Policy

## Supported Versions

Security updates are applied to the latest release on `main` (`0.21.x`).

| Version | Supported |
| ------- | --------- |
| `0.21.x` (latest `main`) | :white_check_mark: |
| `< 0.21.0` | :x: |

## Security & Trust Model

SUSI is designed with a governance-first local host architecture:

1. **Zero-Trust Host Contract (`9090–9094`)**:
   - The daemon seeds `~/.susi/api_token` (`0600` permissions, `O_EXCL` regular-file verification) before binding any port.
   - Every HTTP request on ports `9090`, `9091`, `9093`, and `9094` (except `/health`, `/.well-known/agent-card.json` on `9094`, and CORS `OPTIONS` preflight) requires `Authorization: Bearer <token>`.
2. **`local_only` Privacy & Egress Enforcement**:
   - When `privacy_mode` is `local_only` (or if the `privacy_mode` file is damaged, which fails closed to `local_only`), off-host cloud inference, remote MCP servers, external search tools, and unrequested model downloads are blocked.
3. **Credential Redaction**:
   - Host environment credentials (`*_API_KEY`, `*_TOKEN`, `*_SECRET`) and token patterns are redacted before writing to `.susi/last_blackboard.json`, `.susi/last_mission_trace.json`, `memory.jsonl`, and `error_metrics.jsonl`.
4. **Wasm & Command Sandboxing**:
   - Untrusted Tier-0 reflexes (`reflex_<slug>`) execute in a fuel-metered `Wasmi` sandbox (`susi-vendor-wasmtime`) with network capabilities stripped by default.
   - Extension packs (`~/.susi/extensions/<id>/manifest.json`) enforce explicit permission declarations (`filesystem.read`, `filesystem.write`, `network.egress`, `process.exec`), path containment, SHA-256 checksums, and optional Ed25519 signatures.

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, please report suspected vulnerabilities privately via [GitHub Private Vulnerability Reporting](https://github.com/intellibitz/susi/security/advisories/new) on `intellibitz/susi`.

Please include:
- A clear description of the vulnerability and affected crate/endpoint (e.g., `susi-daemon`, `susi-gmcp`, `susi-gemi`, `susi-sandbox`, `susi-config`).
- Minimal reproduction steps or a proof-of-concept test case.
- Potential impact (e.g., bearer-token bypass, `local_only` egress bypass, path traversal, credential leak in telemetry, or Wasm sandbox escape).

We aim to acknowledge vulnerability reports within **48 hours** and publish a verified fix and regression test as quickly as possible.
