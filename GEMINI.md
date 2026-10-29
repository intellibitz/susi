# susi: read this first

Before you change anything in this repository, do these, in order:

1. Run `susi workflow check` (no installed susi: `scripts/seed-target.sh; cargo run -q -- workflow check`; every worktree builds into a cargo target of its own, so plain cargo is correct once `scripts/seed-target.sh` has run there. Not in the primary checkout while that script says worktrees still share its `target/`: made before the seeding, they build into it until they next merge `origin/main`, and a crate one of them built can run in place of main's; cargo's lock keeps the builds intact, not apart. Then go straight to step 2: `scripts/susi-worktree.sh` runs no cargo).
2. If it reports "own worktree" or "up to date" failures, run `scripts/susi-worktree.sh`
   (no name needed; or `susi workflow start`), then continue inside the directory it
   prints (`cd <path>`). In a worktree just made (`susi workflow start`, the desktop app,
   any `git worktree add`), run `scripts/seed-target.sh` once, in the foreground, before
   your first plain cargo: a hook may still be copying the warm target snapshot in the
   background, and a cargo that does not wait for it builds cold. It waits for that copy,
   replaces a `target` link an older `susi workflow start` (0.21.23) left, and returns at
   once when `target` already holds a build. Never work in the primary checkout or on
   `main`. In a cloud session (Claude Code on the web) the clone you are in is your
   worktree: `scripts/susi-worktree.sh` prints `cd` back into it, and the session never
   asks for the operator's laptop (Remote Control, a computer folder, a button) — see
   `docs/cloud-sessions.md`.
3. Sync with `origin/main`, then take one task: `susi tasks list`, then
   `susi tasks claim <id>`. One live claim at a time. End every commit message
   with the trailer `Task: <id>`.
4. Sync again (`git fetch && git merge origin/main`), then push; sync again before
   the next claim. The gate and every other rule are in AGENTS.md (its START HERE
   block is the single source — read it now).
