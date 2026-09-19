# SUSI Project Contributors

SUSI is an autonomous entity maintained by a decentralized collaboration of substrate collaborators and independent nodes:

## Substrate Collaborators & Independent Nodes
- **IntelliBitz** ([@intellibitz](https://github.com/intellibitz)) - Founder & Lead Maintainer
- **Muthu Ramadoss** ([@muthuramadoss](https://github.com/muthuramadoss)) - Author & Contributor
- **Gemini (Google AI)** ([@gemini](https://github.com/gemini)) - AI Co-Collaborator, AI Vision Architect & Autonomous Contributor
- **Claude (Anthropic)** - AI Co-Collaborator & Autonomous Contributor

## Development Setup

After cloning, enable the project's git hooks (one-time):

```bash
git config core.hooksPath .githooks
```

This activates `pre-push`, which requires a `Cargo.toml` version bump for
any push that touches `src/`, and keeps `README.md`'s version badge and the
`.agents/*.md` version headers in sync with it.
