//! Portable Git-native feature and release distillation engine.
//!
//! Reads `.agents/tasks/*.json` task manifests and optional `susi-feature-manifest-v1`
//! specs directly from Git commits, computes bounded transitive dependency closures,
//! rejects cycles, missing manifests, disallowed SPDX licenses and out-of-scope diffs,
//! and selectively composes candidate branches via 3-way `git merge-tree --write-tree
//! --merge-base` without leaking unselected commits from development `main`.
//!
//! Produces a deterministic, content-addressed `susi-distillation-receipt-v1` receipt
//! recording every `included`, `satisfied_in_base`, `deferred` and `rejected` task with
//! structured reasons, creates local branches via compare-and-swap, and optionally
//! publishes immutable `refs/tags/susi-distill/<name>` snapshots with post-push
//! remote verification. Full release qualification gates remain separate.

use std::collections::BTreeSet;
use std::path::PathBuf;

pub(in crate::distill_compose) use crate::distill::{functional, hostmerge, mandate74};

mod cli;
mod compose;
mod git_ops;
mod receipts;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
use self::{cli::*, compose::*, git_ops::*, receipts::*};

pub use self::receipts::run;

pub(in crate::distill_compose) type Result<T> = std::result::Result<T, String>;

pub const RECEIPT_SCHEMA: &str = "susi-distillation-receipt-v1";
pub const FEATURE_SCHEMA: &str = "susi-feature-manifest-v1";
pub const PLAN_SCHEMA: &str = "susi-distill-plan/v1";
pub(in crate::distill_compose) const MAX_CLOSURE_DEPTH: usize = 256;
pub(in crate::distill_compose) const ZERO_SHA1: &str = "0000000000000000000000000000000000000000";
pub(in crate::distill_compose) const ZERO_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

pub(in crate::distill_compose) const USAGE: &str = "usage: cargo run --offline --locked -p xtask --bin local-distill -- <plan|compose|orchestrate> --name <name> --base <rev> [--from <rev>] [--select <TASK_ID>...] [--tasks-from-range <A..B>] [--feature <path-or-rev:path>] [--plan-manifest <path>] [--functional-only] [--verify-accept] [--publish-snapshot] [--release-candidate] [--pull] [--strict] [--adopt-host-commit <full-commit-id>]... [--receipt-out <path>] [--release-tag <vX.Y.Z>] [--release-record-out <path>] [--json]\n       cargo run --offline --locked -p xtask --bin local-distill -- <publish|status> <name> [--json]\n       cargo run --offline --locked -p xtask --bin local-distill -- release-record --name <name> --tag <vX.Y.Z> [--commit <oid>] [--tagged <iso8601>] [--toolchain <ver>] [--host <triple>] [--asset <file>:<target>:<package>:<bin>:<sha256>]... [--out <path>] [--json]\n       cargo run --offline --locked -p xtask --bin local-distill -- submit-main [--rev <rev>] [--max-attempts <n>] [--json]\n       cargo run --offline --locked -p xtask --bin local-distill -- verify-harvest [--rev <rev>] [--range <A..B>] [--strict] [--json]\n       cargo run --offline --locked -p xtask --bin local-distill -- quarantine-fetch <url> <sha> --slug <slug> [--json]\nDistillation composes an audited candidate from labelled contributions. Release candidates stay local; publish pushes only immutable snapshot tags. Full release qualification gates remain separate.";
pub(in crate::distill_compose) const RELEASE_BRANCH_STORAGE_ONLY: &str = "release branch publication is disabled: release candidates are local-only; GitHub is storage only; use `publish <name>` for an immutable refs/tags/susi-distill/<name> snapshot";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::distill_compose) enum CommandMode {
    Plan,
    Compose,
    Orchestrate,
    Publish,
    Status,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct DistillArgs {
    pub(in crate::distill_compose) mode: CommandMode,
    pub(in crate::distill_compose) name: String,
    pub(in crate::distill_compose) base_rev: String,
    pub(in crate::distill_compose) from_rev: Option<String>,
    pub(in crate::distill_compose) selected: Vec<String>,
    pub(in crate::distill_compose) tasks_from_range: Option<String>,
    pub(in crate::distill_compose) feature_spec: Option<String>,
    pub(in crate::distill_compose) plan_manifest: Option<PathBuf>,
    pub(in crate::distill_compose) functional_only: bool,
    pub(in crate::distill_compose) verify_accept: bool,
    publish_snapshot: bool,
    pub(in crate::distill_compose) release_candidate: bool,
    pub(in crate::distill_compose) pull: bool,
    pub(in crate::distill_compose) strict: bool,
    /// Host-created commits the distilling agent explicitly adopts (full ids);
    /// every adoption is written into the receipt.
    pub(in crate::distill_compose) adopt_host_commits: BTreeSet<String>,
    pub(in crate::distill_compose) receipt_out: Option<PathBuf>,
    pub(in crate::distill_compose) release_tag: Option<String>,
    pub(in crate::distill_compose) release_record_out: Option<PathBuf>,
    pub(in crate::distill_compose) json_output: bool,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct InspectArgs {
    pub(in crate::distill_compose) mode: CommandMode,
    pub(in crate::distill_compose) name: String,
    pub(in crate::distill_compose) json_output: bool,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct SubmitMainArgs {
    pub(in crate::distill_compose) rev: Option<String>,
    pub(in crate::distill_compose) max_attempts: usize,
    pub(in crate::distill_compose) json_output: bool,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct VerifyHarvestArgs {
    pub(in crate::distill_compose) rev: Option<String>,
    pub(in crate::distill_compose) range: Option<String>,
    pub(in crate::distill_compose) strict: bool,
    pub(in crate::distill_compose) json_output: bool,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct QuarantineFetchArgs {
    pub(in crate::distill_compose) url: String,
    pub(in crate::distill_compose) sha: String,
    pub(in crate::distill_compose) slug: String,
    pub(in crate::distill_compose) json_output: bool,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct ReleaseAssetSpec {
    pub(in crate::distill_compose) file: String,
    pub(in crate::distill_compose) target: String,
    pub(in crate::distill_compose) package: String,
    pub(in crate::distill_compose) bin: String,
    pub(in crate::distill_compose) sha256: String,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct ReleaseRecordArgs {
    pub(in crate::distill_compose) name: String,
    pub(in crate::distill_compose) tag: String,
    pub(in crate::distill_compose) commit: Option<String>,
    pub(in crate::distill_compose) tagged: String,
    pub(in crate::distill_compose) toolchain: Option<String>,
    pub(in crate::distill_compose) host: Option<String>,
    pub(in crate::distill_compose) assets: Vec<ReleaseAssetSpec>,
    pub(in crate::distill_compose) out: Option<PathBuf>,
    pub(in crate::distill_compose) json_output: bool,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) enum ParsedCli {
    Help,
    Distill(DistillArgs),
    Inspect(InspectArgs),
    ReleaseRecord(ReleaseRecordArgs),
    SubmitMain(SubmitMainArgs),
    VerifyHarvest(VerifyHarvestArgs),
    QuarantineFetch(QuarantineFetchArgs),
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct Endpoint {
    pub(in crate::distill_compose) address: String,
    pub(in crate::distill_compose) digest: String,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct TaskManifest {
    pub(in crate::distill_compose) id: String,
    pub(in crate::distill_compose) key: String,
    pub(in crate::distill_compose) path: String,
    pub(in crate::distill_compose) title: String,
    pub(in crate::distill_compose) goal: String,
    pub(in crate::distill_compose) owner: String,
    pub(in crate::distill_compose) deps: Vec<String>,
    pub(in crate::distill_compose) scopes: Vec<String>,
    pub(in crate::distill_compose) accept_cmd: Vec<String>,
    pub(in crate::distill_compose) source_commits: Vec<String>,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct FeatureManifest {
    pub(in crate::distill_compose) id: String,
    pub(in crate::distill_compose) title: String,
    pub(in crate::distill_compose) tasks: Vec<String>,
    pub(in crate::distill_compose) deps: Vec<String>,
    pub(in crate::distill_compose) destination_branch: Option<String>,
    pub(in crate::distill_compose) license: String,
    pub(in crate::distill_compose) enabled_capabilities: Vec<String>,
    pub(in crate::distill_compose) disabled_capabilities: Vec<String>,
}

/// Tasks and stream commits a release base already carries through earlier
/// distillations (`Distilled-Task:` / `Source-Commits:` trailers).
#[derive(Debug, Default)]
pub(in crate::distill_compose) struct DistilledInBase {
    pub(in crate::distill_compose) tasks: BTreeSet<String>,
    pub(in crate::distill_compose) source_commits: BTreeSet<String>,
}

impl DistilledInBase {
    pub(in crate::distill_compose) fn covers_commit(&self, commit: &CommitInfo) -> bool {
        self.source_commits.contains(&commit.oid)
            || commit
                .cherry_picked_from
                .iter()
                .any(|oid| self.source_commits.contains(oid))
    }
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct CommitInfo {
    pub(in crate::distill_compose) oid: String,
    pub(in crate::distill_compose) parent: String,
    pub(in crate::distill_compose) subject: String,
    pub(in crate::distill_compose) task_trailers: Vec<String>,
    pub(in crate::distill_compose) cherry_picked_from: Vec<String>,
    pub(in crate::distill_compose) changed_paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::distill_compose) enum VisitState {
    Visiting,
    Visited,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct IncludedEntry {
    pub(in crate::distill_compose) task_id: String,
    pub(in crate::distill_compose) title: String,
    pub(in crate::distill_compose) goal: String,
    pub(in crate::distill_compose) owner: String,
    pub(in crate::distill_compose) deps: Vec<String>,
    pub(in crate::distill_compose) scopes: Vec<String>,
    pub(in crate::distill_compose) accept_cmd: Vec<String>,
    pub(in crate::distill_compose) source_commits: Vec<String>,
    pub(in crate::distill_compose) composed_commit: String,
    pub(in crate::distill_compose) composed_tree: String,
    pub(in crate::distill_compose) patch_id: Option<String>,
    pub(in crate::distill_compose) changed_files: Vec<String>,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct BaseSatisfiedEntry {
    pub(in crate::distill_compose) task_id: String,
    pub(in crate::distill_compose) reason_code: &'static str,
    pub(in crate::distill_compose) detail: String,
}

#[derive(Clone, Debug)]
pub(in crate::distill_compose) struct DecisionEntry {
    pub(in crate::distill_compose) task_id: String,
    pub(in crate::distill_compose) source_commits: Vec<String>,
    pub(in crate::distill_compose) deps: Vec<String>,
    pub(in crate::distill_compose) reason_code: &'static str,
    pub(in crate::distill_compose) detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::distill_compose) enum TaskOutcome {
    Included,
    SatisfiedInBase,
    SkippedNonFunctional,
    Deferred,
    Rejected,
}
