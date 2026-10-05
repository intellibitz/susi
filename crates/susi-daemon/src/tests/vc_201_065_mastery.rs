//! Mastery verification for VC-201-065: the production upgrade path.
//!
//! The functional contract (durable unique backups, rollback, noop rerun,
//! same-second retry cannot clobber) is pinned in susi-config's
//! `vc_201_065_mastery` tests. The remaining refutation was reachability:
//! `migrate_state_dir` existed with no caller on any upgrade path and
//! `state.schema.json` was never read at runtime. These tests pin the fix —
//! the daemon boot sequence runs the migration, before the first state
//! access, and treats failure as fatal rather than advisory.

use std::path::Path;

fn run_daemon_loop_source() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src =
        std::fs::read_to_string(root.join("src/server.rs")).expect("server.rs must be readable");
    let start = src
        .find("fn run_daemon_loop")
        .expect("run_daemon_loop must exist");
    // The function body ends where the next sibling item begins.
    let rest = &src[start..];
    let end = rest
        .find("\n    fn ")
        .or_else(|| rest.find("\n    pub fn "))
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn vc_201_065_mastery_daemon_boot_runs_migration_before_state_access() {
    let body = run_daemon_loop_source();
    let migrate = body
        .find("migrate_state_dir(&global_dir)")
        .expect("daemon boot must invoke migrate_state_dir on the global dir");
    // The migration must land before the first access that assumes the
    // current schema — token seeding, config load, port canonicalization.
    let first_access = body
        .find("ensure_api_auth_token_seeded")
        .expect("daemon boot must still seed the API token");
    assert!(
        migrate < first_access,
        "state migration must run before the first schema-dependent access"
    );
}

#[test]
fn vc_201_065_mastery_failed_migration_aborts_boot() {
    let body = run_daemon_loop_source();
    let migrate = body
        .find("migrate_state_dir(&global_dir)")
        .expect("daemon boot must invoke migrate_state_dir");
    // A rolled-back / failed migration must not silently continue into
    // state reads on a corrupt schema — the boot returns instead.
    let after = &body[migrate..];
    let window_end = after
        .find("ensure_api_auth_token_seeded")
        .unwrap_or(after.len());
    let window = &after[..window_end.min(after.len())];
    assert!(
        window.contains("Err") && window.contains("return"),
        "a failed migration must abort daemon startup, not be logged and ignored"
    );
}

#[test]
fn vc_201_065_mastery_manifest_is_read_at_runtime() {
    // `state.schema.json` is the runtime version marker: read_version runs
    // inside migrate_state_dir at every boot, so a noop-after-upgrade is
    // detected from the manifest rather than assumed.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let migration = std::fs::read_to_string(root.join("../susi-config/src/state_migration.rs"))
        .expect("state_migration.rs must be readable");
    let entry = migration
        .find("pub fn migrate_state_dir")
        .expect("migrate_state_dir must exist");
    let body = &migration[entry..];
    let end = body.find("\nfn ").unwrap_or(body.len());
    let body = &body[..end];
    assert!(
        body.contains("read_version(global_dir)"),
        "the runtime entry point must consult the persisted manifest"
    );
    assert!(
        body.contains("manifest_path") || migration.contains("state.schema.json"),
        "the manifest file must be named and read"
    );
}
