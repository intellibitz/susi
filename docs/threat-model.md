# Threat model: the open core socket

This document states what the free Apache-2.0 core protects, against whom, and
with which mechanism. Every claim below names the code that backs it, and every
mechanism is paired with the boundary it does not cross. Where the code and an
older description disagree, the code wins and the gap is recorded here.

It was written on 2026-11-08 against the tree that lands with task
`T-THREAT-1`, after an external review noted that the pattern-based content
vetoes were presented as a safety boundary while the documents admitted that
content checks stay pattern-based, and that no document covered LAN discovery
(UDP 9092), gossip (UDP 9095), the bearer-token boundary on 9090-9094, or the
self-signed TLS on non-loopback binds.

Scope: the host daemon (`susid`), the thin `susi` client, the four pillar
crates and the leaf services as they ship in this repository. Plugs admitted
over the socket, and everything in Susi Commercial Apps, have their own threat
models.

## Summary

- The daemon binds `127.0.0.1` by default. On that posture the only socket
  reachable from another machine is swarm gossip (UDP 9095), which rejects
  every datagram not sealed under this host's `cluster.key`.
- Every HTTP contract port (9090, 9091, 9093, 9094) requires a bearer token
  or a roster-bound Ed25519 request signature on every request, including
  requests from loopback. `/health`, CORS preflight and the A2A agent card
  are the only unauthenticated routes.
- The bearer token, the bus secret, the MAC signing key, the audit key and the
  cluster key are files owned by the user who runs the daemon. Any process
  running as that user can read all of them. The security boundary of the
  core is therefore the operating-system user account, not the daemon.
- The governance vetoes (Safety and Security detectors) are defense in depth
  against known-dangerous shapes. They are not a boundary against an
  adversarial tool, an adversarial model, or an attacker who already holds
  the bearer token.
- Core ships no working Docker sandbox. The in-process Wasm cell is the only
  execution sandbox in core, and it bounds reflexes, not shell commands.

## Assets

| Asset | Where it lives | Role |
|-------|----------------|------|
| Host API bearer token | `api_token` in the config directory (`SusiDirs::config_dir()`), 0600 on Unix, installed with exclusive create (`SusiConfig::ensure_api_auth_token_seeded`, `crates/susi-config/src/config.rs`) | Authorizes every HTTP request on 9090-9094 and the config and sandbox leaf routes |
| Cluster membership key | `cluster.key` in the config directory, 0600, 32 random bytes hex-encoded (`crates/susi-config/src/cluster_key.rs`) | Signs discovery pings and pongs, seals gossip, seals commit records, derives the shared peer bearer |
| Node identity key | `node.key` (Ed25519) and `node_id` in the config directory | Proves which member signed a request or record once the roster binds its public key |
| MAC signing key and grants | `mac.hmac.key`, `mac.grants/` and `privacy_mode` under the substrate home (`SusiDirs::substrate_home()`; `crates/susi-core/src/mac_policy.rs`) | Signs capability tokens; holds the privacy posture every cell process reads |
| Audit chain key | `audit.hmac.key` under the substrate home (`crates/susi-sandbox-client/src/audit_chain.rs`) | Authenticates the append-only audit log |
| Bus secret | `bus/secret` under the cache directory (`SusiDirs::cache_dir()`; `plane_bus_ipc::bus_secret`) | Required in `X-Susi-Bus-Key` on every loopback plane-bus request |
| Roster and ledger | `peers.json`, `peers_banned.json`, `commit_log.jsonl`, `term.json` beside the config | Cluster membership and the replicated commit history |
| Governance patterns | `config.json` in the config directory, falling back to the compiled `config/config.default.json` | The pattern lists the vetoes match against |
| Workspace and substrate data | the caller's working tree; models, reflexes, memory and traces under the data directory | What the tools read and write |
| Provider credentials | environment variables and the credential store | What `local_only` and the Security detector try to keep on the host |

The directory roles differ by platform (XDG directories on Linux, the Windows
profile directories on Windows, or one `~/.susi` root under `SUSI_HOME`).
`susi os --json` prints the resolved paths.

## Trust boundaries

### Loopback, LAN, internet

- **Loopback (`127.0.0.1`, `::1`)** is the default bind for 9090-9094 and the
  fixed bind for the leaf services (18080-18084) and the plane bus. A
  loopback listener is reachable by every process on the host, whatever user
  it runs as. Reaching it is not the same as being authorized: the HTTP
  ports still demand the bearer and the bus still demands its secret.
- **LAN** becomes reachable for 9090-9094 only when the operator sets
  `bind_address` to an interface address or a wildcard (`0.0.0.0`, `::`).
  A specific address is bound alongside loopback so local callers keep
  working (`SusiDaemon::bind_tcp_canonical`). UDP 9092 binds on the same
  `bind_address`, so on the default posture there is no LAN discovery
  surface at all.
- **Gossip UDP 9095** binds `0.0.0.0:<port>` regardless of `bind_address`,
  and falls back to `0.0.0.0:0` when the port is taken
  (`composition::bind_swarm_gossip`). It is the one socket open to every
  interface on the default posture. Every datagram is HMAC-SHA256 sealed
  under a key derived from `cluster.key`; unsealed or mismatched datagrams
  are counted and dropped (`GossipManager::ingest`).
- **Internet** is never a bind the code chooses. It is reached only as a
  client: provider calls, STUN, model downloads, remote MCP servers and
  remote A2A peers, each gated by the privacy posture described below.

`mac_policy::url_stays_local` treats loopback, RFC 1918, unique-local and
link-local addresses, `localhost`, `*.localhost` and `*.local` as local.
The egress gate therefore never blocks a LAN target. `local_only` means
"nothing leaves the LAN", not "nothing leaves the host", and every surface
that describes it says so: `susi privacy status` and the `privacy_status`
tool report a `network_scope` ("this machine and its local network only"),
the mode-change reports carry the same field, and the CLI help and the GMCP
tool descriptions name the local network. Core has no host-only mode.

### Same user, other users, root

Every secret above is a file the daemon's user can read, and most are
0600 on Unix (the Windows build restricts the bus secret's ACL; the other
files rely on the profile directory's inheritance). Consequences:

- A process running as the daemon's user can read the bearer, call every
  tool, read and write the grants directory, change `privacy_mode`, mint
  capability tokens with the MAC key, append to the audit chain with a
  valid MAC, seal gossip and commit records, and impersonate the node. The
  core does not distinguish such a process from the operator.
- A process running as another local user cannot read the key files, but
  can reach every loopback listener. It is held off by the bearer and bus
  checks described next, and by nothing else.
- Root or an administrator is outside the model: nothing in core resists
  it.

### Operator versus an agent holding the keys

The core's tools are driven by models. A model that calls `exec_command`,
`write_file` or `privacy_grant` acts with the authority of the bearer that
reached the socket, and the bearer is the operator's. In particular:

- `privacy_grant` is an ordinary tool (`crates/susi-gmcp/src/tools/host_ops.rs`).
  Any caller that holds the bearer can grant `process.exec`,
  `network.egress` or `cloud.inference` to the `susi` subject and lift
  the `local_only` restrictions, or call `privacy_mode` to change the
  posture outright. The privacy posture is a guard against mistakes and
  against tools that follow the rules; it is not a guard against a caller
  who holds the bearer and wants it changed.
- Mission subjects are narrower: `ToolRegistry::dispatch`
  (`crates/susi-tools/src/registry.rs`) checks the process posture first and
  then the mission's own token set, so a mission's tokens can narrow what the
  host allows but never widen it. The narrowing holds only for tool calls
  that are attributed to a mission.
- Tool output is wrapped as untrusted content (`untrusted_content.rs`) and
  credential requests inside it are redacted before it reaches a sink. This
  limits what a retrieved document can exfiltrate through result channels;
  it does not stop a model from acting on instructions it read.

## Listening ports and what authenticates them

| Port | Surface | Bind | Authentication | Unauthenticated routes |
|------|---------|------|----------------|------------------------|
| 9090 | GMCP / MCP HTTP (`/mcp`, `/messages`) | `bind_address` (+ loopback) | `NetGuard::is_authorized`: host bearer, cluster peer bearer, or a roster-bound member signature over method, path and body hash; then a per-IP rate limit (120 per minute, IPv6 bucketed per /64), waived for the host's own bearer over loopback | `GET /health`, `OPTIONS` |
| 9091 | GEMI HTTP (OpenAI-compatible, Anthropic Messages, `/metrics`, `/.well-known/susi`) | same | same (`crates/susi-server/src/lib.rs`) | `GET /health`, `OPTIONS` |
| 9092 | A2A UDP discovery | `bind_address` | Only bound `SUSI_PING_SIG3` datagrams are answered: the HMAC under `cluster.key` verifies, the advertised source IP equals the datagram's, the timestamp is within 60 s and the nonce is unseen (`cluster_key::verify_bound_ping`); the retired unbound `SUSI_PING_SIG` / `SUSI_PING_SIG2` and anything else get silence (`start_udp_discovery_server`) | none: an unsigned probe gets silence |
| 9093 | GMCP HTTP alias | same as 9090 | same as 9090 | same |
| 9094 | A2A HTTP (JSON-RPC `/`, SSE `/stream`) | same | the same `NetGuard` verifier, receiving the buffered body so body-bound signatures verify (`server.rs`, `a2a_verifier`) | `/.well-known/agent-card.json` |
| 9095 | Swarm gossip UDP | `0.0.0.0` always | HMAC-SHA256 seal under `HMAC(HMAC(cluster.key, "susi-gossip-v1"), "susi-gossip-peer-v1:<destination id>")`; unsealed datagrams rejected | none |
| 18080 | `susi-paths` leaf | loopback only | none on its routes (path and port lookups) | all |
| 18081 | `susi-error` leaf | loopback only | `SUSI_HOST_TOKEN` when the daemon spawned it; open when started by hand without that variable | `/healthz` |
| 18082 | `susi-config` leaf | loopback only | host bearer, fail closed (`auth::host_token`) | `/healthz` |
| 18083 | `susi-sandbox` leaf | loopback only | host bearer, fail closed | `/healthz` |
| 18084 | `susi-native` leaf (`/wasm/execute`) | loopback only | `SUSI_HOST_TOKEN` when the daemon spawned it; open when started by hand without that variable (`susi_paths::supervisor_bearer_authorized`) | `/healthz` |
| ephemeral | plane bus (`IpcPlaneBus`) | `127.0.0.1:0` per process | `X-Susi-Bus-Key` equal to `bus/secret` on every `/handle` and `/stream` request; 256 concurrent connections, 64 MiB body cap | none |

All five contract ports and the leaf ports shift together under
`port_offset` / `SUSI_PORT_OFFSET`; the layout never changes shape and never
falls back to an ephemeral port. A pinned offset is bound exactly or the
start refuses. With nothing pinned, a foreign holder of one of the six block
ports (contract plus gossip) moves the whole block to the first free offset
of 100 up to 2000 (`crates/susi-daemon/src/port_fallback.rs`); the daemon
logs the holder and publishes the offset with its pid in `ports.json`.
Clients trust that file only while its pid holds the instance's substrate
lock, so a stale file left by a crashed daemon cannot redirect them, and
only a process of the same user can write the config directory it lives in.
The bearer token still guards every contract port, wherever the block
lands.

TLS on the contract ports (`crates/susi-daemon/src/tls.rs`,
`susi_http_transport::dual_transport`):

- Every accepted connection is sniffed by its first byte. A TLS ClientHello
  is upgraded when a certificate is available; anything else is plain HTTP
  on the same port.
- A certificate is available when `tls_cert_path` and `tls_key_path` are
  configured, or when `bind_address` is not loopback, in which case a
  self-signed certificate is generated once under `<config>/tls/` with
  SANs for localhost, the bind address and the hostname. A loopback bind
  with no configured certificate serves plain HTTP only.
- Plaintext from a non-loopback peer is refused unless the operator sets
  `allow_remote_plaintext: true` (`tls::remote_tls_required`). The compiled
  default is `false`, an absent or non-boolean value refuses, and the
  retired `https_only` key is ignored with a one-time notice: the config
  healer had written its old default (`false`) into every existing
  `config.json`, so its value could not tell an operator's choice from the
  healer's, and honouring it would have left every upgraded install open.
  Loopback plaintext always passes.
- A self-signed certificate authenticates nothing to the client unless the
  client pins it, and the peer channel (`susi_core::mcp_client`) still dials
  only `http://<addr>/mcp`: TLS with a certificate pinned from the attested
  roster entry needs a certificate fingerprint carried by the pong and the
  `member_add` record, which no wire format has yet. The client therefore
  never sends a bearer off-host (`mcp_client::credential_for_target`): a
  loopback target gets the credential it was handed; a remote target with
  no node key bound to its address in `peers.json` is refused before a
  connection opens; a remote member with a bound key is called with the
  member signature (method, path, body hash, timestamp, nonce) and a body
  sealed to its key, and no `Authorization` header. A cluster that spans
  hosts therefore works only between members that have bound each other's
  keys and set `allow_remote_plaintext: true`; otherwise its calls fail
  closed with an error that says why.

## What the bearer token and the bus secret protect, and what they do not

**The host bearer** (`api_token`) is the one credential that turns a TCP
connection into authority over the tools. It protects against:

- other local users and sandboxed local processes that can open a loopback
  connection but cannot read the daemon user's files;
- remote hosts that can reach a non-loopback bind;
- a browser page on the same host: CORS is `allow_origin: *`, but a page
  cannot attach the bearer it has never seen, and a request without one is
  refused.

It does not protect against:

- any process running as the daemon's user, which reads the file;
- a caller that was given the token once; there is no per-tool or per-client
  scoping of the host bearer, and a holder may call `privacy_grant`;
- interception on a plaintext path an operator opened with
  `allow_remote_plaintext: true`: a remote client that sends the host bearer
  over `http://` exposes it to the network path. Core's own peer client
  never does (see the TLS section), and signed member requests carry no
  reusable credential, because the signature covers method, path, a body
  hash, a timestamp within 300 s and a nonce the receiver records
  (`net_guard::signed_request_valid`).

Before the token is seeded, requests without a bearer are accepted from
loopback only; the daemon seeds the token before any contract port binds and
aborts if the file cannot be secured.

**The cluster peer bearer** is `HMAC(cluster.key, "susi-peer-bearer-v1")`,
identical on every member and never written to disk. `NetGuard` accepts it
unless the source address is a banned member or a member whose node key is
bound, in which case the member must sign. A stolen `cluster.key` yields this
bearer and with it tool access on every member that has not bound its keys;
full revocation is a cluster re-key.

**The bus secret** (`bus/secret`) protects the plane bus from other local
users: a loopback connection without the secret gets no handler. It does not
protect against the daemon's own user (who reads the file), and the bus
listeners are never exposed beyond loopback. Without OS entropy no secret is
minted and the empty secret matches nothing, so the bus fails closed rather
than falling back to a guessable value.

## The cluster: discovery, gossip, roster

`cluster.key` is a symmetric membership credential (the operator copies it to
every node that should join). It proves that a signer holds the key, not
which holder signed. `node.key` (Ed25519) closes that gap once a committed
`member_add` binds the member's public key in every receiver's roster;
until then, member attribution is self-asserted.

**Discovery (UDP 9092).** A scout sends a bound ping (`SUSI_PING_SIG3`) to
the broadcast address and to each candidate; its MAC covers the sender's
source IP on that route (found by connecting a throwaway UDP socket), its
HTTP alias port, a timestamp and a nonce. A daemon that verifies it records
the advertised address as an `Explicit` peer with a synthetic id and an
empty public key, and answers with a signed pong echoing the ping's nonce
and carrying its verified roster. Properties that hold:

- an unsigned probe is dropped and learns nothing, not even that a daemon is
  listening;
- a captured ping does not verify from any address but the one it names, is
  refused outside 60 s of its timestamp, and is refused a second time inside
  the window: the receiver keeps a bounded nonce cache (4,096 entries,
  entries expire after twice the window, and a full cache refuses rather
  than forgets), recording a nonce only after the MAC, source and timestamp
  verify;
- the retired unbound pings (`SUSI_PING_SIG`, `SUSI_PING_SIG2`) are neither
  answered nor admitted, even with a valid MAC; a daemon logs the first one
  it drops. Members must all run a build that speaks `SUSI_PING_SIG3`;
- a synthetic row earns no credential: the peer client releases a credential
  only to loopback or to an address whose node key is bound in the roster,
  and mission dispatch to an unbound row fails naming the missing
  attestation (`mcp_client::credential_for_target`);
- a captured pong cannot be replayed against a different ping, because the
  echoed nonce is part of the MAC;
- a banned member gets neither admission nor a pong.

Properties that do not hold, recorded as residual risk:

- a host that spoofs the UDP source address of a real member can replay
  that member's ping within the window to a receiver that has not seen it;
  the receiver then admits the member's own address, which it would admit
  anyway, and the pong goes to the member, not the spoofer. The nonce cache
  lives in memory, so a daemon restarted inside the window forgets it; the
  source binding still holds;
- the pong discloses the responder's node id and roster (ids and addresses)
  to any holder of `cluster.key`.

**Gossip (UDP 9095).** Capability advertisements are sealed with a
destination-bound key so a datagram for A cannot be replayed onto B, but any
holder of `cluster.key` can mint a valid seal for any destination; this is
destination binding, not a per-peer secret (`gossip.rs`). The persisted
`gossip_caps.json` is sealed under a store key and an unsigned file is
ignored. Gossip content is capability names; it carries no tool authority.

**The replicated ledger** (`commit_log.rs`) seals every record under
`cluster.key` and, once keys are bound, under the coordinator's `node.key`.
Privileged records (roster deltas, re-key) additionally need a bound
majority's endorsements and must come from a current member acting as
leader. So a stolen `cluster.key` alone can mint HMAC-valid decision records
and pre-binding history, but can no longer add, remove or re-key members. A
stolen `node.key` lets its holder sign as that member, which is one vote and
one leadership claim; a roster change still needs the bound majority's
endorsements.

## What the HMAC chains prove, and against whom

**The audit chain** (`audit_chain.rs`): each entry is hash-linked to the
previous tip and MACed under `audit.hmac.key`. `verify_chain` detects any
rewrite, deletion or reordering of recorded lines by a party that does not
hold the key; the `chain.tip` checkpoint carries the last hash forward so a
writer never links to a stale tip. It proves nothing against the daemon's own user, who holds the key and can
rewrite the file and re-MAC every line. It is tamper evidence for an operator
reviewing a host they trust, and evidence for nobody else unless the key is
escrowed elsewhere. Secrets are redacted before entries are written
(`redact_credentials`), with the limits of the pattern list below.

**The commit ledger** proves membership (the sealer held `cluster.key`) and,
for records after key binding, identity (the coordinator held its
`node.key`). It does not prove that the committed value was correct or that
the vote was honest; it proves who committed what, in which term, in which
order.

**Capability tokens** (`mac_policy.rs`) are HMAC-SHA256 over subject, action,
resource and expiry under `mac.hmac.key`. They prove that a holder of the key
issued the grant. Every cell process shares that key, and the daemon's user
can read it, so a token is a record of an operator decision, not a
credential a cell could not forge.

## What the sandboxes bound

**The Wasm cell** (`crates/susi-vendor-wasmtime`, which links the `wasmi`
interpreter despite its compatibility name) runs Tier 0 reflexes and
`/wasm/execute` payloads with: fuel metering (`CELL_FUEL`), memory capped at
256 MiB however large a module declares, 1,000,000 table elements, 64 MiB
module size, 16 MiB output, 5 s wall time, and a WASI preview1 host that
exposes no filesystem, environment, stdin, sockets or process handles
(`wasi.rs`). Only binary `.wasm` is accepted. A reflex can therefore burn its
fuel and return bytes; it cannot touch the host. Reflex modules are files
under the data directory, so the same-user boundary applies to who can place
one.

**Docker `sandbox_exec` has no implementation in core.** The GMCP tool
forwards to the sandbox leaf's `POST /sandbox/docker_exec`, and that handler
(`crates/susi-leaf-services/src/sandbox_svc.rs`) discards the command and
returns 500; no crate in this workspace depends on `bollard`. Consequences:

- `sandbox_exec` always fails in core;
- under `local_only`, or with `mandatory_sandbox_for_exec`, `exec_command`
  is redirected to that path and so is refused unless the caller holds
  `process.exec`. The mandatory sandbox posture in core means "host exec is
  refused", not "host exec is isolated".

**`exec_command`** spawns without a shell after `shlex` splitting and checks
the first word against `ALLOWED_EXEC_BINS` (`crates/susi-gawd-agents/src/safety.rs`),
with extra rules for the flags that turn `find`, `fd`, `rg`, `rustc`, `git`
and `sed` into exec or write primitives. The list contains `bash`, `sh`,
`python`, `python3`, `curl`, `wget`, `git`, `gh`, `make` and `cargo`, so a
command line can run anything those programs run. The code's own comment
says it: "this list is not a sandbox". Confinement of `exec_command` is the
operating-system user account and the privacy posture, nothing finer.

`write_file` and `read_file` resolve through the deepest existing directory
of the caller's workspace and refuse to climb out of it
(`confined_workspace_join`). A socket caller names its workspace as an
absolute path; `MacPolicy::confine_workspace` accepts any existing
directory other than a filesystem root on which the subject holds a
`filesystem.write` (or `.read`) grant. The seeded grant is the wildcard, so
a bearer holder can name the config directory itself as a workspace and
rewrite `config.json`, including the governance pattern lists; an operator
who wants the workspace jail to mean something replaces the wildcard with
grants on specific trees (`privacy_revoke` then `privacy_grant`).

## Governance vetoes: defense in depth, not a boundary

The action-capable tools pass their command or goal text through
`SafetyDetector::audit_action` and `SecurityDetector::audit_action`
(`crates/susi-gawd-agents/src/safety.rs`, `security.rs`), wired through
`EngineHooks::audit_action`; an unwired context fails closed. The audited
callers are `exec_command`, `sandbox_exec`, mission goals (`SUSI_SOLVE`),
`reason`, `agents_run`, the `os_*` host tools, `native_disk_usage` and the
cluster record tools (`commit_record`, `member_propose`, `member_endorse`,
`cluster_rekey_commit`). `write_file` is confined by the workspace jail
described above, not by these detectors. The checks are:

- recursive `rm` of `/`, `~`, `$HOME` in any flag spelling (a tokenizer, not
  a substring match);
- the configured `destructive_commands`, `critical_system_paths`,
  `exfiltration_vectors` and `secret_tokens` lists, matched at token starts;
- the value of any `*_API_KEY`, `*_TOKEN` or `*_SECRET` the process holds.

These are pattern checks over the text of a command or goal. They catch the
known-dangerous shapes they name, with lower false-positive rates than a
bare substring match. They do not and cannot:

- bound what an allowlisted interpreter does (`bash -c`, `python -c`, a
  script file, a `Makefile`);
- recognize an encoded, split, aliased or indirect form of a listed command;
- stop exfiltration through a tool the list never names, or through a
  program's own network features;
- reason about intent; a benign-looking command with a harmful effect
  passes.

An adversarial tool, an adversarial model or a caller holding the bearer
should be assumed able to get past them. The vetoes reduce the blast radius
of ordinary mistakes and of the most common prompt-injection payloads, and
they produce audit evidence when they fire. They are not the mechanism that
keeps an untrusted agent off the host; that mechanism is the user account
the daemon runs as, the privacy posture, and the operator's choice of which
clients receive the bearer.

## The privacy posture, as implemented

`PrivacyMode` (`mac_policy.rs`) has three values. What each seeds for the
`susi` subject (`seed_defaults`):

| Mode | `filesystem.read/write`, `sandbox.exec`, `transmit app://*` | `process.exec` | `network.egress` | `cloud.inference` |
|------|---------------------------------------------------------------|----------------|------------------|-------------------|
| `balanced` (default) | granted | granted | granted | granted |
| `open` | granted | granted | granted | granted |
| `local_only` | granted | not granted; `exec_command` refused (see above) | not granted | not granted |

`balanced` and `open` differ only in `blocks_network_by_default`: in
`balanced` an egress tool still has to find a grant, and the seeded wildcard
grant is that grant until the operator revokes it. The `require_egress_consent`
key in `config.json` is read by nothing. The practical posture is therefore:
`local_only` keeps traffic on the host and LAN; the other two permit egress
and cloud inference out of the box.

Posture state is a sticky file (`privacy_mode`) every cell process reads; a
damaged file enforces `local_only`. Switching to `local_only` revokes the
wildcard egress, cloud and exec grants in the same call.

Which tools count as egress is decided by name
(`requirements_for_tool`): an explicit list (`a2a_delegate`, `brave_search`,
`harness_run`, ...) plus substrings (`browser`, `mcp_`, `download`, `http`,
`fetch`, `web_search`). A tool whose name matches none of these is treated as
a local read. The egress gate is exactly as good as that classification; a
plug that reaches the network under an unlisted name is not gated by it.
Direct provider calls, model downloads and the peer channel call
`egress_permitted` themselves.

On every boot in a posture that permits egress, the daemon sends STUN binding
requests to `stun.l.google.com:19302` and `stun.cloudflare.com:3478` to learn
its public address (`composition::spawn_nat_discovery`). Under `local_only`
it does not, unless `SUSI_TURN_SERVER` or `SUSI_TURN_RELAY` is set.

## Residual risks, in one place

1. Same-user processes are the operator. Nothing in core distinguishes them.
2. The shared peer bearer is a symmetric credential; evicting a member
   without a re-key leaves it able to authenticate from a fresh address.
3. The peer channel has no TLS client. Cross-host peer calls carry no
   bearer, so no credential crosses the network, but they work only between
   members that bound each other's keys and set `allow_remote_plaintext`,
   and their request and response headers (not the sealed bodies) are
   readable on the path.
4. Gossip 9095 is open on every interface regardless of `bind_address`.
5. Egress classification is by tool name; `local_only` permits the whole LAN
   (and says so in every report); core has no host-only mode.
6. `balanced` grants egress and cloud inference by default; the config key
   that reads as a consent requirement is dead.
7. Governance vetoes are pattern checks (above).
8. Core has no Docker sandbox; `exec_command` confinement is the user account.
9. Leaf services 18081 and 18084 are unauthenticated when started by hand
    without `SUSI_HOST_TOKEN`; the daemon always passes the token when it
    spawns them.

## Recommended operator posture

- **Keep `bind_address` at `127.0.0.1`** unless a remote client needs the
  socket. On that posture the daemon's only off-host surface is gossip 9095,
  and gossip is inert without a shared `cluster.key`.
- **Run the daemon as a dedicated user** when other local users or untrusted
  local software share the machine. The bearer, the MAC key and the audit
  key are only as private as that account's files.
- **Treat the bearer as the operator credential it is.** Hand it only to
  clients you would let run `exec_command`. Rotate it by replacing
  `api_token` and restarting; there is no finer scope.
- **Use `local_only`** when the host must not talk to the internet, and
  remember it still talks to the LAN and still refuses host exec rather than
  isolating it. Revoke the wildcard grants explicitly if you stay in
  `balanced` but want egress consent per tool
  (`susi privacy revoke` / `privacy_revoke`).
- **Use `port_offset`** to run a second instance or to move the socket off
  the canonical ports; it shifts all ports together and is advertised in
  discovery so members still find each other.
- **Bind beyond loopback only with a plan for certificates.** The
  self-signed certificate is generated for you but authenticates nothing a
  client does not pin. Supply `tls_cert_path` / `tls_key_path` from a CA the
  clients trust; remote plaintext is refused unless you set
  `allow_remote_plaintext: true`. A cluster that spans hosts needs that
  setting on every member today (the peer client has no TLS yet) and works
  only between members whose keys are bound, so put it on a network segment
  you control.
- **Share `cluster.key` deliberately** and only with hosts you would let
  hold the peer bearer. Bind member keys (`member_add`) so bearer-only calls
  from member addresses are refused, and re-key to evict.
- **Read the audit chain as tamper evidence against others**, not as proof
  against the account that wrote it. If you need the latter, copy
  `audit.hmac.key` and the chain tip to a place the daemon's user cannot
  write.
- **Do not rely on the vetoes to contain a tool you do not trust.** Admit
  only plugs you would run as the daemon's user, because that is what they
  are.

## Out of scope

Plugs and extension packs (they run with the authority they are admitted
with; their own documents describe their boundaries), the commercial desktop
and its multi-user controls, physical and hypervisor attacks, and supply-chain
integrity of the toolchain, which the license and build gates cover
separately.
