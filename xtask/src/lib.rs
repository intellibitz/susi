//! Shared `xtask` library crate consolidating release, workflow, distillation,
//! qualification, and provenance logic so binaries and tests do not recompile
//! or re-execute duplicate module graphs.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::Path;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Runs `git` inside `root` with literal pathspecs and replacement objects
/// disabled, returning trimmed UTF-8 stdout.
pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    common::git_trimmed(root, args)
}

pub mod affected;
pub mod build_time;
pub mod common;
pub mod compiler_env;
pub mod distill;
pub mod distill_compose;
pub mod extension_watch;
pub mod fast_release;
pub mod gate_receipt;
pub mod git_publish;
pub mod gmcp_roadmap;
pub mod harvest;
pub mod local_build;
pub mod local_workflow;
pub mod native_json;
pub mod native_provenance;
pub mod native_test_counts;
pub mod product_features;
pub mod push_policy;
pub mod release_lanes;
pub mod release_qualification;
pub mod release_version_guard;
pub mod storage_guard;
pub mod sysroot_sources;
pub mod test_binary_evidence;
#[cfg(test)]
pub mod test_fixture_fs;
pub mod test_runner;
pub mod trusted_sysroot;
pub mod vendor_regressions;
pub mod workspace_test_count;
pub mod workspace_test_proof;
pub mod worktree_admin;
