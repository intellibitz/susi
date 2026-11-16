# Susi authors and maintainers

Susi core is an Apache-2.0 open-source project of IntelliBitz. It is published
one way: IntelliBitz's CEO alone holds push authority, exercised by the CEO or
by the agents the CEO authorizes. No pull request, patch or commit from outside
is accepted, so this file lists the people and agents who write what is
published here. Anyone is free to fork the repository and work in their own
fork; see **[CONTRIBUTING.md](CONTRIBUTING.md)**.

## Maintainer and authors

- **IntelliBitz** ([@intellibitz](https://github.com/intellibitz)) — Founder & Lead Maintainer
- **Muthu Ramadoss** ([@muthuramadoss](https://github.com/muthuramadoss)) — Author & Principal Architect
- **Gemini (Google AI)** — AI Co-Collaborator, AI Vision Architect & Autonomous Contributor
- **Claude (Anthropic)** — AI Co-Collaborator & Autonomous Contributor
- **The other agents the CEO authorizes** write, test and integrate the code
  under [AGENTS.md](AGENTS.md). Each commit names its task
  (`Task: T-<AGENT>-<n>`); the record of who did what is in
  [`.agents/tasks/`](.agents/tasks/) and the Git history, which also keeps the
  authorship of every earlier commit.

## Harvested open-source work

Susi reuses Apache-2.0 work from other projects. Their authors are credited
where the code is: in each harvest manifest
(`.agents/harvest/H-<AGENT>-<n>.json`), in the notices kept in the harvested
files, and in the `THIRD_PARTY_NOTICES` that ship with a release
([docs/harvest.md](docs/harvest.md)).

## Development setup

After cloning (once per clone; worktrees share it):

```bash
scripts/setup-dev.sh
```

This enables `.githooks` (`pre-commit`: staged Rust formatting; `pre-push`:
transport safety only) and the `ledger` merge driver that unions entries
appended to `.agents/evidence.json` by parallel agents. The CEO's agents work in
isolated worktrees (see **[AGENTS.md](AGENTS.md)**, "Native local agent loop").
