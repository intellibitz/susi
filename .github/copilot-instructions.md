# susi workflow (read first)

Before you change anything in this repository, do these, in order:

1. Run `susi workflow check` (no installed susi: `scripts/target-guard.sh --run cargo run -q -- workflow check`; worktrees share one cargo target, and the guard keeps a build from another worktree out of it).
2. If it reports "own worktree" or "up to date" failures, run `scripts/susi-worktree.sh`
   (no name needed; or `susi workflow start`), then continue inside the directory it
   prints (`cd <path>`). Never work in the primary checkout or on `main`. In a cloud
   session (Claude Code on the web) the clone you are in is your worktree: the script
   prints `cd` back into it, and the session never asks for the operator's laptop
   (Remote Control, a computer folder, a button) — see `docs/cloud-sessions.md`.
3. Sync with `origin/main`, then take one task: `susi tasks list`, then
   `susi tasks claim <id>`. One live claim at a time. End every commit message
   with the trailer `Task: <id>`.
4. Sync again (`git fetch && git merge origin/main`), then push; sync again before
   the next claim. The gate and every other rule are in AGENTS.md (its START HERE
   block is the single source — read it now).
