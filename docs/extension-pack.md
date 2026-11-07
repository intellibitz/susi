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
   `stdout`:

```rust
use std::io::{self, Read, Write};

fn main() -> io::Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let trimmed = input.trim();
    let bytes = trimmed.len();
    let response = format!(
        "{{\"status\":\"ok\",\"reflex\":\"word_count\",\"bytes\":{bytes}}}\n"
    );
    io::stdout().write_all(response.as_bytes())
}
```

```bash
cargo build --target wasm32-wasip1 --release
install -m 0644 target/wasm32-wasip1/release/word_count.wasm ~/.susi/reflexes/word_count.wasm.tmp
mv ~/.susi/reflexes/word_count.wasm.tmp ~/.susi/reflexes/word_count.wasm
```

2. Copy the compiled `<slug>.wasm` into your `reflexes/` directory via atomic
   rename (stage to `<slug>.wasm.tmp` then `mv` to `<slug>.wasm`).
3. Call it over MCP on port `9090` (`tools/call` with `"name": "reflex_<slug>"`).

### Second example: stateless PII/secret redaction (`pii_redact`)

A practical follow-up to `word_count`: read a JSON payload from `stdin`
(falling back to `argv[1]`, the slot the host sandbox actually wires the tool
arguments to), redact email-shaped tokens and the string values of
secret-bearing JSON keys, and print one deterministic JSON line. It is
stateless, uses only `std`, and the sandbox strips network egress by default,
so the raw payload never leaves the process.

```rust
use std::fmt::Write as _;
use std::io::{self, Read, Write};

/// JSON keys whose string values are treated as secrets and redacted.
const SECRET_KEYS: [&str; 9] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "api-key",
    "authorization",
    "private_key",
];

fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS.contains(&key.trim_matches('"').to_ascii_lowercase().as_str())
}

/// Escape a string for embedding as a JSON string value.
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Index of the closing quote of the JSON string that opens at `start`,
/// or `None` when the string is unterminated.
fn closing_quote(payload: &str, start: usize) -> Option<usize> {
    let bytes = payload.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Replace the string value of every `"secret_key": "…"` pair with
/// `[REDACTED]`, counting the replacements.
fn redact_secret_values(payload: &str, redactions: &mut u64) -> String {
    let bytes = payload.as_bytes();
    let mut out = String::with_capacity(payload.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            let ch = payload[i..].chars().next().unwrap_or('"');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let Some(key_end) = closing_quote(payload, i) else {
            out.push_str(&payload[i..]); // unterminated: pass the tail through
            return out;
        };
        let key = &payload[i..=key_end];
        let mut j = skip_ws(bytes, key_end + 1);
        let has_string_value = j < bytes.len() && bytes[j] == b':' && {
            j = skip_ws(bytes, j + 1);
            j < bytes.len() && bytes[j] == b'"'
        };
        if has_string_value && is_secret_key(key) {
            if let Some(value_end) = closing_quote(payload, j) {
                out.push_str(key);
                out.push_str(":\"[REDACTED]\"");
                *redactions += 1;
                i = value_end + 1;
                continue;
            }
        }
        out.push_str(key);
        i = key_end + 1;
    }
    out
}

/// Split `token` at its single `@` into `(local_part, domain)`; `None` when
/// the token is not a plain `local@domain` shape.
fn email_parts(token: &str) -> Option<(&str, &str)> {
    let at = token.find('@')?;
    if token[at + 1..].contains('@') {
        return None;
    }
    let local = &token[..at];
    if local.is_empty() || local.starts_with('.') || local.ends_with('.') {
        return None;
    }
    Some((local, &token[at + 1..]))
}

/// `true` for dotted alphanumeric/hyphen domains with an alphabetic TLD of
/// at least two characters (`example.com`, `sub.example.co`; not `a@b`).
fn valid_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    let Some((tld, rest)) = labels.split_last() else {
        return false;
    };
    !rest.is_empty()
        && tld.len() >= 2
        && tld.bytes().all(|b| b.is_ascii_alphabetic())
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Replace email-shaped tokens with `[REDACTED_EMAIL]`, counting them.
fn redact_emails(payload: &str, redactions: &mut u64) -> String {
    // A token is a maximal run of address characters (including `@` so
    // `local@domain` stays one token); non-address bytes pass through.
    let is_word_char =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-' | b'@');
    let bytes = payload.as_bytes();
    let mut out = String::with_capacity(payload.len());
    let mut i = 0;
    while i < bytes.len() {
        if !is_word_char(bytes[i]) {
            let ch = payload[i..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word_char(bytes[i]) {
            i += 1;
        }
        let token = &payload[start..i];
        if let Some((_, domain)) = email_parts(token) {
            if valid_domain(domain) {
                out.push_str("[REDACTED_EMAIL]");
                *redactions += 1;
                continue;
            }
        }
        out.push_str(token);
    }
    out
}

fn main() -> io::Result<()> {
    // The host sandbox wires no stdin (EOF) and passes the payload as
    // argv[1]; a plain WASI runner piping stdin works too, so try stdin
    // first and fall back to the argument when it is empty.
    let mut piped = String::new();
    let _ = io::stdin().read_to_string(&mut piped);
    let payload = match piped.trim() {
        "" => std::env::args().nth(1).unwrap_or_default(),
        trimmed => trimmed.to_owned(),
    };
    if payload.trim().is_empty() {
        return io::stdout().write_all(
            b"{\"status\":\"error\",\"reflex\":\"pii_redact\",\"error\":\"empty input\"}\n",
        );
    }
    let mut redactions = 0u64;
    let redacted = redact_secret_values(&payload, &mut redactions);
    let redacted = redact_emails(&redacted, &mut redactions);
    let response = format!(
        "{{\"status\":\"ok\",\"reflex\":\"pii_redact\",\"redactions\":{redactions},\"output\":\"{}\"}}\n",
        json_escape(&redacted)
    );
    io::stdout().write_all(response.as_bytes())
}
```

```bash
cargo build --target wasm32-wasip1 --release
install -m 0644 target/wasm32-wasip1/release/pii_redact.wasm ~/.susi/reflexes/pii_redact.wasm.tmp
mv ~/.susi/reflexes/pii_redact.wasm.tmp ~/.susi/reflexes/pii_redact.wasm
```

Test it with a tiny harness that pipes sample payloads over `stdin`/`stdout`
and asserts on the redacted output. `wasmtime run` inherits stdin/stdout, so
the first mode exercises the real `wasm32-wasip1` artifact; `RUNNER=cargo`
checks the same fixtures against a native host build (the reflex is std-only,
so it compiles unchanged for both targets):

```bash
#!/usr/bin/env bash
# test_reflex.sh - pipe sample payloads over stdin/stdout and assert the
# redaction contract of the pii_redact reflex.
#
# Usage (from the reflex's cargo project directory):
#   RUNNER=wasmtime ./test_reflex.sh   # run the compiled wasm, stdin piped
#   RUNNER=cargo    ./test_reflex.sh   # native host build via `cargo run`
set -euo pipefail

WASM="${WASM:-target/wasm32-wasip1/release/pii_redact.wasm}"
RUNNER="${RUNNER:-wasmtime}"

run() {
  local payload="$1"
  case "$RUNNER" in
    cargo) cargo run --quiet -- "$payload" ;;                # argv[1] input
    *) printf '%s' "$payload" | "$RUNNER" run "$WASM" ;;     # stdin input
  esac
}

pass=0
fail=0
check() {
  local name="$1" payload="$2" want="$3" got
  got=$(run "$payload")
  if [[ "$got" == *"$want"* ]]; then
    pass=$((pass + 1))
    echo "PASS $name"
  else
    fail=$((fail + 1))
    echo "FAIL $name: got $got"
  fi
}

check "email redacted" \
  '{"user":"jane@example.com","city":"Bhopal"}' \
  '"output":"{\"user\":\"[REDACTED_EMAIL]\",\"city\":\"Bhopal\"}"'
check "secret values redacted" \
  '{"authorization":"Bearer sk-live-abc123","ok":true}' \
  '"output":"{\"authorization\":\"[REDACTED]\",\"ok\":true}"'
check "clean payload untouched" \
  '{"city":"Bhopal","n":42}' \
  '"output":"{\"city\":\"Bhopal\",\"n\":42}"'

echo "$pass passed, $fail failed"
[[ $fail -eq 0 ]]
```

Expected output:

```text
PASS email redacted
PASS secret values redacted
PASS clean payload untouched
3 passed, 0 failed
```

Call it over MCP on port `9090` like any other reflex (`tools/call` with
`"name": "reflex_pii_redact"`); the JSON line is deterministic, so the
`redactions` count and the redacted `output` string are safe to assert on.

See also `ARCHITECTURE.md` → Admission protocols and [docs/integrating-with-susi.md](integrating-with-susi.md).
