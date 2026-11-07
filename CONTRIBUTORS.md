# SUSI Project Contributors

SUSI is an Apache-2.0 open-source project maintained by a collaborative community of open-source engineers, protocol implementers, extension-pack authors, and autonomous agent nodes.

Want to join the contributor list? Read **[CONTRIBUTING.md](CONTRIBUTING.md)** or pick up an open issue labeled [`good first issue`](https://github.com/intellibitz/susi/labels/good%20first%20issue) or [`help wanted`](https://github.com/intellibitz/susi/labels/help%20wanted)!

## Maintainers, Human Contributors & Autonomous Nodes
- **IntelliBitz** ([@intellibitz](https://github.com/intellibitz)) — Founder & Lead Maintainer
- **Muthu Ramadoss** ([@muthuramadoss](https://github.com/muthuramadoss)) — Author & Principal Architect
- **Community Contributors** — Open-source engineers, MCP/A2A integrators, and extension-pack authors ([see all GitHub contributors](https://github.com/intellibitz/susi/graphs/contributors))
- **Gemini (Google AI)** ([@gemini](https://github.com/gemini)) — AI Co-Collaborator, AI Vision Architect & Autonomous Contributor
- **Claude (Anthropic)** — AI Co-Collaborator & Autonomous Contributor

## Development Setup

After cloning (once per clone; worktrees share it):

```bash
scripts/setup-dev.sh
```

This enables `.githooks` (`pre-commit`: rustfmt; `pre-push`: clippy) and the
`ledger` merge driver that unions entries appended to `.agents/evidence.json`
by parallel agents. External contributors can submit standard GitHub Pull Requests
from a fork (see **[CONTRIBUTING.md](CONTRIBUTING.md)**); local multi-agent workers
use isolated worktrees (see **[AGENTS.md](AGENTS.md)**, "Native local agent loop").
