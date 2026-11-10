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

`CapabilityRegistry::dispatch_capability_in` and a `delegation` argument on the
`os_capabilities` MCP tool and `susi os capabilities invoke --delegation <json>`
let a driver in another process continue the nesting: it sends
`DelegationScope::envelope()` (`susi-delegation/v1`: path, permissions,
remaining budget and time, limits), and the receiver rebuilds the ancestry with
`DelegationScope::from_envelope`, so depth and cycle checks keep counting.
Malformed, cyclic, too-deep or over-limit envelopes and unknown fields are
refused.

## Limits (not claimed)

- **Envelopes are unsigned.** They bound cooperating components. A caller can
  present any envelope, just as it can present any `permission` to the bus
  today; authority over the socket still comes from the host MAC policy.
  Signing envelopes with a node key is future work.
- Cancellation does not cross an envelope; the remaining deadline does.
- Budget accounting is per process. A remote child's spending is not refunded
  to, or charged against, the sender's in-memory scope.
- Only capability dispatch uses the contract. Harness missions, GEMI worker
  delegation and GAWD swarm hand-offs do not open scopes yet; wiring each is
  separate work.
- Deadlines are checked when a scope is opened or `check()`ed; a running
  handler is not pre-empted. The existing elapsed-time check still applies.

## Verification

```text
cargo test --offline --locked -p susi-core --lib -- delegation capability_bus registry
cargo test --offline --locked -p susi-gmcp --lib -- os_capabilities
cargo clippy --offline --locked -p susi-core -p susi-gmcp -p susi-cli-defs --all-targets -- -D warnings
```
