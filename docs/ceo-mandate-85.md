# CEO Mandate 85: No Agent Holds a Claim After It Is Done With It

Effective 2026-11-16. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 85**. It sharpens rule 2 of
[Mandate 83](ceo-mandate-83.md) ("hold no claim you are not working on") at the
point where work ends, and it wins where older text differs.
`intellibitz/susi-commercial-apps` carries the paired commercial edition in its
own `docs/ceo-mandate-85.md`, beside the workflow tool that holds the rule.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 3 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 85 (`docs/ceo-mandate-85.md`):
the moment you are done with a task, its claim ends; finish with
`susi-commercial workflow finish <id>`, or release it with
`susi-commercial tasks release <id>` when you stop without finishing; never keep
a claim to reserve files or let it run out its lease; a claim on done work
holds nothing.

## 0. The CEO's words

> make a mandate that an agent cannot hold a claim, after it is done with it

Recorded as vision `V-10` here.

## 1. The rule

1. **Done means done.** An agent is done with a claim when the task's
   completed record is on `main`, when it hands the task off, releases or
   abandons it, or when it stops working on it for any reason: its session
   ends, it turns to other work, or it is blocked and moves on.
2. **The claim ends in the same step that ends the work.** Finish with
   `susi-commercial workflow finish <id>`, which accepts, publishes and
   releases in one go, or publish the completed record with
   `susi-commercial workflow ship`, which releases a finished task's claim.
   When you stop without finishing, release it with
   `susi-commercial tasks release <id>`; work you committed is published or
   given up with `--abandon <reason>`, never dropped silently.
3. **Never hold a claim for later.** Not to reserve files, not to keep a task
   for a session that may come back, and not until its lease runs out.
4. **A claim on done work holds nothing.** A claim on a task whose completed
   record is on `main` is released, whatever store it is in and whether or not
   anyone has removed it yet: it reserves no files, blocks no other agent and
   is not shown as held. Any agent's tool removes such a leftover.
5. **The lease is the last resort, not the release.** A claim nobody releases
   lapses after its one-hour lease (Mandate 83 rule 3). Relying on that is a
   breach of this mandate, not a way to keep it.
6. **Look before you leave.** Before an agent ends a session, it lists what it
   still holds (`susi-commercial workflow check`) and releases every claim on
   work it is done with.

## 2. What it changes

- **Mandate 83 rule 2** said "release when you stop". Mandate 85 says when that
  is, makes a claim on finished work void for every tool, and removes the lease
  as an excuse.
- **Mandate 76**: a Handoff or Landed item on the board ends the claim it names
  at the same time; it is never posted while the claim is kept.
- Nothing changes in how a claim is taken, renewed or reserved while the work
  is still going.

## 3. Implementation truth

- **Measured when the mandate was set (2026-11-16):** of the task claim refs
  on the remotes, 19 in core and 16 of 17 in `susi-commercial-apps` were on
  tasks whose completed record was already on `main`. None was being removed:
  nothing in the workflow tool deleted a claim ref once its task was done. One
  of them was still live for hours on a finished task.
- **Enforced today, in `susi-commercial-apps`' workflow tool:**
  `workflow ship` releases the publisher's claim on a task whose completed
  record it carried to `main`; `workflow finish` releases the claim in the
  same push that publishes the work; claiming new work does not count a claim
  whose task is done on `main` as reserving files.
- **Being built in `susi-commercial-apps`:** reading every claim on done work
  as released in every listing, and deleting leftovers at the next queue sync
  on a host that may write the queue's refs. The commercial edition records
  when that lands.
- **Not enforced:** rules 1, 3 and 6 for work that is not finished. The tool
  cannot see that an agent has stopped or turned to other work; the one-hour
  lease is what catches it.
- The self-build contract (`crates/susi-core/src/self_build.rs`) carries the
  rule to every delegated coding task in a Susi checkout, with no line added
  to core (Mandate 84).
