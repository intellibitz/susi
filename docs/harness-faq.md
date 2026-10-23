# Harness FAQ

## Why a metering gateway at all?

The gateway is what a budget needs: attribution and price at the moment of the
call. It writes one ledger row per call — mission, task, attempt, model, tokens,
cache hits, the price in force — before it answers. That row is what the budget
guard reads, what the cockpit sums, and what the model ranking learns from. A
provider's own dashboard cannot say which task spent the money.

It also routes: the built-in registry knows each provider's endpoint, key
variable and model capabilities, so the request goes straight to the provider
over HTTPS. Nothing sits between the gateway and the model.

## Does a cache hit still cost money?

No. The gateway's own cache answers the request and marks the response; the
ledger records the row with `cached: true` and `cost: 0`. The token counts are
kept because they describe the answer that was served, not because they were
billed.

## How is the budget enforced?

Before a turn, the guard computes a pessimistic estimate: the prompt at the
cache-miss rate, the full output allowance at the output rate. If spent money,
in-flight reservations and that estimate cross the mission cap, the turn is not
dispatched. It is recorded as skipped and the task is left for a top-up. The
estimate is deliberately too high, never too low.

## Why can two concurrent tasks not overspend together?

`BudgetGuard` holds one reservation pool. Each admitted turn reserves its
estimate; the second turn sees the first reservation and is admitted only if
both fit. The ledger's actual charge replaces the reservation after every turn.

## When does a failure get another model?

A transient provider failure (429, overload, connection) gets one retry on the
same model. A timeout or a semantic failure escalates immediately. An I/O error
stops. If two different models fail with the same signature, the task is
stuck: that is evidence about the task or the environment, not about model
capability, and more attempts would just buy the same failure.

## Why reset the worktree between models?

A rejected attempt may have committed work that fails acceptance. Publishing it
would be wrong, and letting the next model build on it would contaminate the
comparison. The adapter commits the task record first, then resets to that
commit before the next model. The claim and the task file survive; the rejected
work does not.

## What does "cost-ranked" mean exactly?

Models are ordered by expected cost per **accepted** task:

```
expected = (calls / successes) × (average cost of an attempt)
```

A model with no ledger history is ranked at its list price, so a new cheap model
gets a chance. A model that is cheap but never succeeds is ranked behind every
model that does succeed. The ranking is recomputed from the ledger, so it
improves as the harness runs.

## Is the cockpit read-only?

It is read-mostly. The panel reads one snapshot and never writes the ledger or
the event feed. The one write path is `POST /api/command`, which appends an
operator command to the control log; the supervisor applies it at the next task
boundary. Pause and abort are cooperative: the current turn finishes, then no
new task starts. Killing a turn mid-call still pays for its tokens and leaves a
worktree nobody can account for.

## Can I embed it in another harness desktop?

Yes. The panel is a single HTML page with no build step and no external assets.
Any desktop that can host an iframe can embed `http://127.0.0.1:9181/?embed=1`;
the JSON API is the contract for a native surface. This repository ships a
DeepSeek Harness plugin at `deploy/dsh-cockpit` as the reference integration: a
sidebar icon plus a main panel, installed through the plugin manager.

## Why does the panel say "waiting for the cockpit"?

The iframe has not loaded yet, or the server is not running. Start
`susi harness cockpit … --port 9181` in the mission workspace. The panel's URL is
editable and the setting is stored in the browser, so a cockpit on another port
works without reinstalling the plugin.

## Embedding the cockpit in another desktop

The panel is one HTML page with no build step. Embed `http://127.0.0.1:9181/?embed=1` in an iframe; the JSON API at `/api/status` is the contract for a native surface. The DeepSeek Harness plugin in `deploy/dsh-cockpit` is the reference: a sidebar entry and a main panel around the same page.

