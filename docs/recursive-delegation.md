# Recursive driver/engine delegation: first slice

Mandate 73 asks that any Susi component can be a driver (it hands work to
children) and an engine (it does work a parent handed it), nested to any depth,
with identity, permissions, budgets, cancellation, errors and cycle checks
preserved at every level. Task `T-CLAUDERECURSE-1` implements the contract for
**capability dispatch**, the one path every surface (MCP, A2A, HTTP, adapters)
already shares. It does not claim that every Susi component uses it yet.

## What is implemented

`susi_core::delegation::DelegationScope` is one level of nesting.
`DelegationScope::delegate` is the only way to open the next level, and it
refuses, leaving the parent unchanged, when:

| Refusal | Rule |
| --- | --- |
| `PermissionAmplification` | a child holds a subset of its parent's permissions |
| `BudgetExceeded` | a child's budget is reserved from the parent's remaining budget |
| `DepthExceeded` | depth below the root is bounded (default 16, ceiling 256) |
| `FanOutExceeded` | children opened over a scope's lifetime are bounded (default 256), so a loop that keeps re-delegating stops |
| `Cycle` | a `(component, capability)` hop already on the ancestry cannot be re-entered; a component may appear again for a different capability (a driver becoming an engine) |
| `Cancelled` | cancelling a scope cancels every descendant, not its parent or siblings |
| `DeadlineExceeded` | a child's deadline is never later than its parent's |
| `InvalidIdentity` | identities are 1 to 256 bytes with no control characters |

Unspent budget returns to the parent when a child is dropped; a child is kept
alive by its own children, so refunds arrive bottom-up. `path()` lists every hop
from the root, so a nested result keeps its provenance.

`CapabilityBus::dispatch_in(parent, request, handler)` opens one child scope per
invocation (hop = request subject and capability id; budget = the call's cost
ceiling; timeout = the call's timeout ceiling) and passes it to the handler. A
handler that dispatches again is a driver nested inside an engine. A result
produced after an ancestor was cancelled is reported as `Cancelled`, not
success. A handler's own cost must fit in what its nested calls left. Receipts
carry `delegation_path` (omitted from JSON when empty, so older receipts still
parse). `CapabilityBus::dispatch` keeps its signature and runs from a root scope
built from the request, so every existing production dispatch now runs under the
contract.

## Signed envelopes across processes (T-CLAUDESIGNENV-1)

A driver in another process continues the nesting with a
`susi-delegation/v1` envelope: path, permissions, budget, limits, a random
128-bit `id`, an absolute `expires_unix_ms` and an HMAC-SHA256 `signature`.
Only a `DelegationAuthority` issues or admits one; there is no public way to
turn an unsigned envelope into a scope.

- **Key.** `DelegationAuthority::host()` derives its key from the host's shared
  MAC policy key (`substrate_home/mac.hmac.key`) with
  `MacPolicy::derive_key(b"susi-delegation-envelope/v1")`, so the daemon, its
  cells and the CLI of one host verify each other's envelopes, and the key
  verifies no other kind of token. A process that only has a private fallback
  key refuses to issue or admit.
- **Issue.** `os_capabilities` with `action: "delegate"` (CLI:
  `susi os capabilities delegate <id> --subject <component> --permission <p>
  [--budget-micros N] [--timeout-ms N] [--delegation <parent>]`) opens the level
  `subject:id` under the parent envelope, or under a fresh root for the subject,
  with every check in the table above, and returns it signed. Its lifetime is
  the scope's remaining time, at most one hour. The issuing process keeps the
  level in its ledger until expiry, so the budget it reserved from its parent
  stays reserved while the envelope can be used.
- **Admit.** `invoke` with `delegation` verifies the signature in constant time
  and the expiry before anything else; an unsigned, tampered (wider
  permissions, larger budget, later expiry, other id, shorter path, deeper
  limits) or foreign-host envelope is `Unverified`, a late one `Expired`.
  Even a correctly signed envelope is rebuilt only if its path, identities,
  limits and permissions are valid.
- **Replay.** Within one process every presentation of an id continues the
  same level: replaying an envelope shares its remaining budget and fan-out
  instead of resetting them. The ledger holds at most 4,096 live envelopes.
- **Revoke.** `DelegationAuthority::revoke(id)` cancels the level and
  everything opened under it in that process; later presentations are refused
  as cancelled.

## Limits (not claimed)

- Any process holding the host key (any Susi process of the same host user)
  is trusted to issue. The envelope is bound to the host key, not to the
  transport or caller presenting it: whoever holds an envelope can use it
  within its bounds until it expires.
- The replay ledger and revocation are per process. Two processes admitting
  the same envelope each start from its full budget; the daemon socket is the
  intended single admitting process. `revoke` has no socket or CLI action yet.
- Envelopes do not cross hosts: another host's key refuses them. A cluster-key
  variant is future work.
- Cancellation crosses an envelope only through revocation in the admitting
  process; the issuer cancelling its own level does not reach another process.
- Only capability dispatch uses the contract. Harness missions, GEMI worker
  delegation and GAWD swarm hand-offs do not open scopes yet.
- Deadlines are checked when a scope is opened or `check()`ed; a running
  handler is not pre-empted. The existing elapsed-time check still applies.

## Verification

```text
cargo test --offline --locked -p susi-core --lib -- delegation capability_bus registry
cargo test --offline --locked -p susi-gmcp --test os_capabilities_delegation
cargo test --offline --locked -p susi-gmcp --lib -- os_capabilities
cargo clippy --offline --locked -p susi-core -p susi-gmcp -p susi-cli-defs --all-targets -- -D warnings
```

The GMCP test re-runs itself as three child processes on scratch `SUSI_HOME`
directories: one issues through `delegate`, one sharing its home admits through
`invoke`, one on another home refuses.
