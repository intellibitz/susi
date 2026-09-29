# SUSI Project Contributors

SUSI is an autonomous entity maintained by a decentralized collaboration of substrate collaborators and independent nodes:

## Substrate Collaborators & Independent Nodes
- **IntelliBitz** ([@intellibitz](https://github.com/intellibitz)) - Founder & Lead Maintainer
- **Muthu Ramadoss** ([@muthuramadoss](https://github.com/muthuramadoss)) - Author & Contributor
- **Gemini (Google AI)** ([@gemini](https://github.com/gemini)) - AI Co-Collaborator, AI Vision Architect & Autonomous Contributor
- **Claude (Anthropic)** - AI Co-Collaborator & Autonomous Contributor

## Development Setup

After cloning (once per clone; worktrees share it):

```bash
scripts/setup-dev.sh
```

This enables `.githooks` (`pre-commit`: rustfmt; `pre-push`: clippy) and the
`ledger` merge driver that unions entries appended to `.agents/evidence.json`
by parallel agents. Work in your own worktree and branch, then `git fetch`,
merge `origin/main`, and push (see AGENTS.md, "Multi-agent parallel work").
