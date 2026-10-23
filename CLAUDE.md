# susi: read this first

Before you change anything in this repository, do these, in order:

1. Run `susi workflow check` (no installed susi: `cargo run -q -- workflow check`).
2. If it reports "own worktree" or "up to date" failures, run `scripts/susi-worktree.sh`
   (no name needed; or `susi workflow start`), then continue inside the directory it
   prints (`cd <path>`). Never work in the primary checkout or on `main`.
3. Sync with `origin/main`, then take one task: `susi tasks list`, then
   `susi tasks claim <id>`. One live claim at a time. End every commit message
   with the trailer `Task: <id>`.
4. Sync again (`git fetch && git merge origin/main`), then push; sync again before
   the next claim. The gate and every other rule are in AGENTS.md (its START HERE
   block is the single source — read it now).
