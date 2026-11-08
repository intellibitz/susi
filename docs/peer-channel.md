# The sealed peer channel

Every byte that travels between two Susi machines travels inside the sealed
peer channel. This document is its normative description: the topology it is
built for, the cryptographic construction, what it protects and what stays
visible, how members are admitted, and what changes for older nodes. The code
it names is the reference: `crates/susi-config/src/peer_channel.rs` (the
handshake and transport ciphers), `crates/susi-config/src/peer_channel_link.rs`
(links, streams, relay, uplinks, the client), `crates/susi-daemon/src/peer_channel_host.rs`
(the daemon's listener, uplink keeper and hand-off to GMCP), and
`crates/susi-core/src/mcp_client.rs` (every peer call). The end-to-end test is
`tests/peer_channel_hub.rs`.

Introduced by task `T-PEERCHAN-1`, after the CEO asked to "connect all 3
laptops using public internet in a safe and secure way" and decided (21:47 IST,
2026-11-08) that reachability is one forwarded router port on one hub laptop.

## Topology: one hub, members behind NAT

```
   member A (no inbound port)                       member B (no inbound port)
        |  dials out, keeps the link open                |  dials out, keeps the link open
        v                                                v
  [ home router of the hub: public-ip:29090  --port forward-->  192.168.0.140:29090 ]
                                     |
                                     v
                     hub (the CEO laptop): susid with peer_channel_listen
```

- **One port reaches the hub from outside**: the channel port, TCP, default
  **29090** (`peer_channel_listen`). The contract block 9090-9095 stays on
  `bind_address` (127.0.0.1 by default) and is never forwarded. Port 29090 is
  outside that block and every offset of it (up to 11095), outside the leaf
  services (18080-18084) and below the common ephemeral ranges; `port_offset`
  never shifts it.
- **Members have no listener.** Each member's daemon dials the hub and keeps
  that one TCP connection open (a PING after 15 s of silence, the link is dead
  after 45 s without a frame, reconnect with exponential backoff from 1 s to
  60 s, the hub's name re-resolved on every attempt). The hub calls a member
  over the link the member opened.
- **Member to member goes through the hub, end to end.** A call from A to B is
  an end-to-end session between A and B that the hub forwards as opaque
  stream frames. The hub holds the keys of its own links to A and to B, never
  the keys of the A-B session.
- **The hub is its key, not its address.** A member stores the hub's node id
  and Ed25519 public key as the identity, and its address (a DNS name, or an
  operator-supplied `ip:port`) as a hint in `peer_channel_hubs.json`. Every
  dial proves the key; a changed, wrong or hijacked address yields a refused
  handshake, never a weaker one. A hub that sets `peer_channel_public` also
  sends its members a signed list of its current addresses, which they store
  as extra dial hints.

### What the CEO applies on the hub

1. Router: forward **TCP 29090** on the public side to **192.168.0.140:29090**
   (the hub laptop's LAN address; reserve it in the router's DHCP table so it
   does not change). Nothing else is forwarded; no UDP.
2. Windows Defender Firewall on the hub, one inbound rule (an elevated
   PowerShell):

   ```powershell
   New-NetFirewallRule -DisplayName "Susi peer channel" -Direction Inbound `
     -Action Allow -Protocol TCP -LocalPort 29090 `
     -Program "$env:USERPROFILE\.susi\bin\susi.exe" -Profile Private
   ```

   `-Profile Private` matches a home network classified as private
   (`Get-NetConnectionProfile` shows the category); use `-Profile Any` only if
   the home network is classified public. The members need no inbound rule.
3. Hub configuration (`config.json`): `"peer_channel_listen": "0.0.0.0:29090"`,
   and optionally `"peer_channel_public": "<public-name-or-ip>:29090"` so the
   hub tells its members where it is. `bind_address` stays `127.0.0.1`.
4. Every machine holds the same `cluster.key` (copied once, by hand, over a
   channel the operator trusts). On each member: `susi peers add
   <public-name-or-ip>:29090`.

The agents do not touch the router, the firewall, or any relay or VPN service.

## Construction

### Handshake

The handshake is the Noise Protocol Framework pattern **`XXpsk0`** with
**X25519**, **ChaCha20-Poly1305** and **SHA-256** (Noise rev. 34), protocol
name `Noise_XXpsk0_25519_ChaChaPoly_SHA256`:

```
XXpsk0:
  -> psk, e
  <- e, ee, s, es
  -> s, se
```

The key schedule is the Noise `SymmetricState` exactly:

- `h = SHA-256("Noise_XXpsk0_25519_ChaChaPoly_SHA256")` (the name is longer
  than 32 bytes), `ck = h`; then `MixHash(prologue)`.
- `HKDF(ck, ikm)` with `HMAC-SHA-256`: `t = HMAC(ck, ikm)`,
  `o1 = HMAC(t, 0x01)`, `o2 = HMAC(t, o1 || 0x02)`, `o3 = HMAC(t, o2 || 0x03)`.
- `MixKey(ikm)`: `ck, k = HKDF(ck, ikm)`, nonce `n = 0`.
- `MixKeyAndHash(ikm)`: `ck, t_h, k = HKDF(ck, ikm)`; `MixHash(t_h)`.
- `MixHash(d)`: `h = SHA-256(h || d)`.
- `EncryptAndHash(p)`: `c = ChaCha20-Poly1305(k, nonce = 0^32 || LE64(n++), ad = h, p)`; `MixHash(c)`.
- Token `psk`: `MixKeyAndHash(psk)`. Token `e`: send/receive the 32-byte
  ephemeral public key, `MixHash(e)`, and, because this is a PSK handshake,
  `MixKey(e)`. Tokens `ee`, `es`, `se`: `MixKey(X25519(...))`, with the
  initiator computing `es = DH(e, rs)`, `se = DH(s, re)` and the responder the
  mirror images. Token `s`: `EncryptAndHash(static public key)`.
- `Split()`: `k1, k2 = HKDF(ck, "")`; `k1` encrypts initiator to responder,
  `k2` responder to initiator.

Every DH output that is all zero (a low-order point) aborts the handshake.

**Static keys are the node identities.** A node's static X25519 key is derived
from its Ed25519 `node.key` by the standard birational map: the secret is the
lower 32 bytes of `SHA-512(seed)` (clamped inside X25519), the public key is
the Montgomery form of the Ed25519 verifying key
(`cluster_key::node_x25519_secret`, `VerifyingKey::to_montgomery`).

**The pre-shared key is the cluster.**
`psk = HMAC-SHA-256(cluster.key, "susi-peer-channel-v1/psk")`. A responder
tries its current key and, during a re-key's overlap window, the previous one.
A first message that opens under neither is answered with silence: the
responder never reveals its identity, or that it is a Susi node, to a host
without the cluster key.

**Domain separation.** Two prologues keep the two layers apart:
`susi-peer-channel-v1/link` for a hop-by-hop link and
`susi-peer-channel-v1/e2e` for an end-to-end session. A message from one layer
never opens in the other. The PSK label, the payload version byte and the
signed-hint prefix `susi-peer-channel-v1/hint:` are distinct from every other
string Susi signs or MACs (`susi-member-v1:`, `susi-bind-v1:`,
`susi-peer-req-v2:`, `susi-peer-bearer-v1`, `susi-peer-enc-v1`).

### Payloads and what is bound to what

- Message 1 payload (encrypted under the PSK-derived key):
  `version (1) | unix seconds (8, BE) | flags (1)`; flag `0x01` means "keep
  this link open to receive calls" (a member's uplink).
- Message 2 and message 3 payloads (encrypted under keys that include
  `ee`/`es` or `ee`/`es`/`se`): `version | unix seconds | id_len | node_id |
  Ed25519 public key (32) | bind_sig (64)`, where `bind_sig` is the node's own
  `susi-bind-v1:{node_id}:{pubkey}` attestation (the value `member_add`
  records carry as `subject_sig`).
- A receiver accepts that payload only if the Montgomery form of the Ed25519
  key equals the static key the `s` token delivered and the DH tokens
  authenticated, and `bind_sig` verifies under that Ed25519 key for that node
  id. The DH proves possession of the static secret; the equality binds that
  secret to the Ed25519 identity; the attestation binds the identity to the
  node id. The node id is in the wire alphabet of `cluster_key`, at most 64
  bytes.
- The handshake hash `h` covers the protocol name, the prologue, both
  ephemeral keys, both encrypted static keys and all three payloads. Both
  sides finish with the same `h` (channel binding).

### Freshness, replay and key confirmation

- The responder refuses message 1 whose timestamp is more than 60 s from its
  clock (the `SUSI_PING_SIG3` window), and records the initiator's ephemeral
  key in a bounded cache (4,096 entries, entries live twice the window, a full
  cache refuses rather than forgets). A verbatim replay of message 1 inside
  the window is refused at the first message. A replayed handshake could not
  complete anyway: the replayer holds neither the initiator's ephemeral nor
  its static secret.
- Identity payloads carry a timestamp too and are refused outside the window.
- Key confirmation: the initiator sends message 3 only after message 2
  decrypted under keys that include the responder's static key (`es`); the
  responder serves nothing before message 3 decrypted under keys that include
  the initiator's static key (`se`). Either side that cannot derive the
  session keys fails the AEAD of the next message.
- Forward secrecy: the session keys depend on `ee` (two fresh ephemerals per
  session); a later compromise of `node.key` or `cluster.key` does not decrypt
  recorded sessions.
- Transport: each direction has its own key and a 64-bit counter nonce
  starting at 0. A replayed, reordered, dropped or injected message fails its
  tag; the stream or link it belongs to is closed, never resynchronized. The
  counter refuses to wrap.

### Framing

On TCP a link is the 8-byte preamble `SUSIPC1\n`, then messages framed as
`u16 BE length | bytes` (at most 65,535 bytes, the Noise limit): the three
handshake messages, then link transport messages. Nothing else is written to
the socket.

Inside a link transport message, a frame is `kind (1) | stream (4, BE) | body`:

| kind | body | meaning |
|------|------|---------|
| OPEN | `ttl | dst_len | dst node id | src_len | src node id` | open a stream to `dst` |
| DATA | one end-to-end message | carry a stream's bytes |
| CLOSE | reason | end a stream |
| PING / PONG | echo | keepalive |
| HINT | `{ts, addresses, sig}` | the sender's signed dial addresses |

The dialer opens odd stream numbers, the acceptor even ones. A link carries at
most 256 streams; a relay decrements `ttl` (starting at 4) and refuses at 1.

### Two layers

- A **link** is one TCP connection with its own `XXpsk0` handshake under the
  link prologue. A link from a node the hub dialed as a hub may label streams
  with other nodes as their source; on a link the hub accepted, the source of
  every stream is the link's proven peer, whatever the frame says.
- Every **call** is an end-to-end session: a second `XXpsk0` handshake under
  the end-to-end prologue, run inside one stream, between the caller and the
  destination. The caller requires the destination's key
  (`Expect::Key`); the destination requires the caller's proven node id to
  equal the stream's source. The session then carries one HTTP/1.1 exchange
  (the request line, headers, body and the whole response, including
  streamed SSE bodies) in messages of at most 60,000 bytes.

There is one code path for every route: even a call to the hub itself is an
end-to-end session inside a link to the hub.

### Routing

A caller (`peer_channel_link::open`) tries, in order: a link this process
already holds to the destination; the destination's own channel endpoints
(`channel_endpoint` in its roster row, or its hub addresses); this process's
uplinks; a short-lived link to each hub in `peer_channel_hubs.json`; and this
node's own daemon's listener. A hub forwards a stream to the persistent link
of its destination, or, for a stream that came up from a member, to its own
uplink. It forwards for this node's own clients and for bound members only;
an identity not bound in its roster may call the hub itself but is never
relayed.

### Where a call lands

The daemon hands a session that terminates at it to its GMCP surface over an
in-process socket pair whose accepted end is checked to be the connecting end
(`peer_channel_host::socket_pair`), together with the identity the session
proved. GMCP authorizes every request on that connection by that identity
(`NetGuard::channel_authorized`): this node's own key or a member bound to
that node id is authorized; an identity that proved the cluster key but is not
bound here has the standing a cluster peer-bearer caller has (it is how
`peers add` and its endorsements run); a banned identity, or one that claims a
bound member's node id or key under another identity, is refused, and so is a
request whose member-signature headers name a different node. Standing is
read again on every request. No bearer token travels between machines. The
request's `Host` is rewritten to the loopback name, since the MCP host
allowlist exists for browsers, which cannot reach this path.

## What is protected, and what is visible

Protected (confidential and authenticated) on the network path: the request
line, every header, the body, the response, which tool is called, the node ids
of both ends of a call and of the link, the stream structure, the time stamps
and the address hints.

Visible on the network path: that a TCP connection exists between two IP
addresses and ports, the 8-byte preamble `SUSIPC1\n` (it marks the protocol
and version), the size and timing of each message, and the first message's
ephemeral public key (random bytes).

Visible to the hub for a call it relays between two members: the stream
number, the source and destination node ids, the size and timing of the
end-to-end messages, and that the stream exists. Not visible to it: anything
inside the end-to-end session. The test drives a member-to-member call through
a real hub daemon with `SUSI_CHANNEL_RELAY_TRACE` set (a diagnostic that
appends every stream body the hub forwards, as the hub reads it after opening
its own link encryption) and finds no request line, header, JSON or node id in
the trace.

A relay or forwarded port in front of the hub sees exactly what the network
path sees.

## Membership is the key, the address is a hint

- `peers.json` binds a node id to an Ed25519 key. The channel admits by
  proven identity; member-signed requests verify by node key and nonce, not by
  source address (`NetGuard::signed_request_valid`). A member whose traffic
  comes from a router's address, or from a new public address, is the same
  member.
- `susi peers add <host:port>` first tries the channel at that endpoint (the
  default port 29090 when none is given): the handshake proves that the
  responder holds the cluster key and which node key it holds, even though the
  address it was reached at is not its own. The endpoint proof
  (`cluster_status` must report the same node id) then runs inside the
  channel, the responder is bound in the roster with that endpoint as its
  address hint, and a node with no channel listener keeps an uplink to it
  (`peer_channel_hubs.json`). A node that is not yet in any cluster joins the
  responder's cluster: it asks the responder's leader (`member_propose`, with
  its own key and attestation) to admit it, and does not start a ledger of its
  own. A node that is already in a cluster commits the admission through its
  own leader as before.
- A member reachable only through a hub is listed in the other rosters under
  the routing name `<node_id>.invalid:<port>`, which never resolves: it is
  reached by its key, over the link it keeps open.
- On one local network, `susi peers add <host>` still falls back to the bound
  discovery handshake (`SUSI_PING_SIG3`) with its source binding, window and
  nonce cache unchanged; the endpoint proof then runs inside the channel to
  the key the pong attested, on that host's channel port.

## Discovery and gossip stay on the local network

UDP discovery (9092) answers datagrams from loopback, RFC 1918, link-local and
unique-local sources only; a datagram from a public address is dropped unread,
whatever the bind. Gossip (9095) binds `bind_address` and drops datagrams from
public sources the same way. Both are zero-configuration conveniences for one
network; across networks, membership is explicit (`peers add` over the
channel) and every call is routed by key, so neither is needed off the local
network, and leaving them unexposed keeps the only internet-facing surface the
channel port. `host_only` still confines both to loopback, and `local_only`
keeps the channel itself on the local network (below).

## Privacy postures

The listener checks each connecting address, and every dial each resolved
address, against the privacy posture before a byte is read or written, with
the same rule the egress gate uses: under `host_only` nothing beyond loopback,
under `local_only` nothing beyond the local network (so a hub on the internet
is unreachable in that mode), under `balanced` and `open` what the egress
grants allow. A posture switch applies to the next connection.

## Upgrade path (v0.21.52 and earlier)

All members upgrade together. Mixed clusters fail closed:

- An old node calling a new node sends plaintext HTTP to its contract port
  from another machine; the new node refuses remote plaintext unconditionally
  (`allow_remote_plaintext` is retired: a `true` left in a config logs a
  one-time notice and opens nothing).
- A new node calling an old node opens the channel to a port where nothing
  listens (or something that is not a channel): the call fails with an error
  that says no handshake answer came back. The new node never falls back to
  plaintext.
- An old node's roster rows (address plus bound key) keep working on a new
  node: the key is the identity; the address becomes a hint.
- Discovery pings stay `SUSI_PING_SIG3` and gossip seals are unchanged, so two
  new nodes on one network discover each other as before.

## Limits

- The channel authenticates nodes, not users: any process that can read a
  node's `node.key` and `cluster.key` is that node (the same-user boundary of
  `docs/threat-model.md`).
- A cluster-key holder that is not bound in a roster can call that node with
  peer-bearer standing, as on a local network today. Revocation remains a
  cluster re-key.
- Traffic analysis (who talks to whom, when and how much) is not hidden.
- The hub is a single point of reachability for members behind NAT: if it is
  down, those members cannot reach each other (they never reach each other
  any other way).
- A2A (9094) and GEMI (9091) are not carried between machines; the peer
  protocol is MCP on GMCP.
