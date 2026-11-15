//! Local-only, cooperative worktree and scope reservations for parallel agents.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[cfg(windows)]
extern crate susi_windows_runtime;

fn main() -> std::process::ExitCode {
    xtask::local_workflow::main_entry()
}
