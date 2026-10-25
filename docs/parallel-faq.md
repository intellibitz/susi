# Parallel FAQ

Preparation runs in parallel; landing is serial.

### Why does a worktree sometimes fail to compile another worktree's changes?

Worktrees share the primary checkout's cargo target directory. Because of that, an artifact built from another checkout can shadow a worktree's sources, so a build can pick up the wrong version of a package.

The harness adapter now runs `cargo clean -p <package>` for the packages a task's scopes touch before its clippy gate. That way the gate compiles what it actually reads, rather than a stale artifact left behind by another worktree.
