#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! SUSI configuration substrate: `SusiConfig` dynamic registry, typed config
//! fragments, and shared self-healing JSON load/merge/save helpers.
//!
//! Runs as a standalone REST service (`127.0.0.1:18082`, see `main.rs`) — the
//! canonical reader/writer for the shared `~/.susi/config.json` while the
//! substrate is up. Every other crate depends on this crate; only the
//! *global* config read/write path prefers the service, so a running
//! substrate stays the canonical writer. When the service is unreachable —
//! or explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) is set, e.g. in
//! tests with a swapped `HOME` — every call resolves against the local files,
//! so config access never hard-fails on service health. `cluster_key`
//! signing/verification is deliberately local only: exposing HMAC over
//! `cluster.key` (0600) as an unauthenticated localhost endpoint would let
//! any process mint signed cluster messages.

pub use susi_error;

pub mod cloud_env;
pub mod cluster_key;
mod config;
pub mod desired_state;
pub mod explain;
pub mod extensions;
pub mod file_lock;
mod json_util;
pub mod key_scope;
pub mod keyring_storage;
pub mod plan_apply;
pub mod rekey_schedule;
pub mod secret_ref;
pub mod selfheal;
pub mod setting_registry;
pub mod setup_workflow;
pub mod state_backup;
pub mod state_migration;
mod types;
pub mod validate;
pub mod versioned_store;
pub mod zc_bare_susi;
pub mod zc_config_overrides;
pub mod zc_debt_report;
pub mod zc_env_classify;
pub mod zc_env_detectable;
pub mod zc_installer;
pub mod zc_path;
pub mod zc_susi_fix;
pub mod zc_uninstall;

/// IPC client for the standalone `susi-config` service. Only the global
/// config path is routed here; per-directory loads and `cluster_key` stay
/// local by construction.
mod service {
    use std::time::Duration;
    use susi_paths::loopback::{self, Auth};

    use super::SusiConfig;

    const DEFAULT_PORT: u16 = susi_paths::ports::CONFIG_SERVICE;
    const TIMEOUT: Duration = Duration::from_millis(200);

    fn port() -> u16 {
        loopback::service_port("SUSI_CONFIG_PORT", DEFAULT_PORT)
    }

    /// The service process, or explicit local env config (`SUSI_XDG`,
    /// `XDG_*_HOME`), resolves locally — a swapped HOME in tests must not
    /// read or write the host substrate's real `config.json`.
    fn local_only() -> bool {
        susi_paths::is_service_mode() || loopback::local_env_override()
    }

    /// `GET /config` — healed global config from the running substrate.
    pub fn get_global() -> Option<SusiConfig> {
        if local_only() {
            return None;
        }
        let resp = loopback::request(
            &loopback::Endpoint {
                port: port(),
                timeout: TIMEOUT,
                auth: Auth::HostToken,
            },
            "GET",
            "/config",
            None,
        )?;
        serde_json::from_str(resp.ok_body()?).ok()
    }

    /// `POST /config` — route a global-dir save through the substrate when
    /// it is up; `false` tells the caller to fall back to the local atomic
    /// write (identical bytes, same file).
    pub fn save_global(cfg: &SusiConfig) -> bool {
        if local_only() {
            return false;
        }
        let Ok(payload) = serde_json::to_string(cfg) else {
            return false;
        };
        loopback::request(
            &loopback::Endpoint {
                port: port(),
                timeout: TIMEOUT,
                auth: Auth::HostToken,
            },
            "POST",
            "/config",
            Some(&payload),
        )
        .is_some_and(|resp| resp.is_success())
    }
}

pub use cloud_env::{cloud_env_overlay, env_or_cloud_env};
pub use config::{redact_credentials, SusiConfig};
pub use desired_state::{
    parse_desired_state, round_trip_desired_state, validate_desired_state, DesiredRef,
    DesiredState, DESIRED_STATE_SCHEMA,
};
pub use explain::{explain_key, explain_key_global, ConfigLayer, ConfigOrigin, ExplainedSetting};
pub use json_util::{
    atomic_replace_file, atomic_write_bytes, atomic_write_json_pretty, clear_json_override,
    confined_workspace_join, create_private_dir, install_private_file, load_or_create_secret,
    merge_missing_json_defaults, merge_missing_registry_defaults, remove_file_if_present,
    write_json_override, DynamicRegistry, DynamicValue, ModelTier, ProviderType, StringRegistry,
};
pub use key_scope::{resolve_key_ref, KeyRef, KeyScope, ScopedSecret};
pub use rekey_schedule::{evaluate_rekey_policy, RekeyPolicy, RekeyReason, RekeyScheduleDecision};
pub use selfheal::{load_or_selfheal, SelfHealReport};
pub use state_migration::{migrate_state_dir, MigrationReport};
pub use types::*;
pub use validate::{
    apply_fixes, validate_dir, validate_settings, ConfigFix, ConfigIssue, ConfigIssueKind,
    ConfigValidationReport,
};
pub use versioned_store::VersionedJsonStore;

#[cfg(test)]
#[path = "tests/cluster_key.rs"]
mod cluster_key_tests;
#[cfg(test)]
#[path = "tests/config_validate.rs"]
mod config_validate_tests;
#[cfg(test)]
#[path = "tests/desired_state.rs"]
mod desired_state_tests;
#[cfg(test)]
#[path = "tests/extensions.rs"]
mod extensions_tests;
#[cfg(test)]
#[path = "tests/json_util.rs"]
mod json_util_tests;
#[cfg(test)]
#[path = "tests/key_scope.rs"]
mod key_scope_tests;
#[cfg(test)]
#[path = "tests/keyring_storage.rs"]
mod keyring_storage_tests;
#[cfg(test)]
#[path = "tests/rekey_schedule.rs"]
mod rekey_schedule_tests;
#[cfg(test)]
#[path = "tests/selfheal.rs"]
mod selfheal_tests;
#[cfg(test)]
#[path = "tests/setup_workflow.rs"]
mod setup_workflow_tests;
#[cfg(test)]
#[path = "tests/state_backup.rs"]
mod state_backup_tests;
#[cfg(test)]
#[path = "tests/state_migration.rs"]
mod state_migration_tests;
#[cfg(test)]
#[path = "tests/vc_201_063.rs"]
mod vc_201_063_tests;
#[cfg(test)]
#[path = "tests/vc_201_065.rs"]
mod vc_201_065_tests;
#[cfg(test)]
#[path = "tests/vc_201_066.rs"]
mod vc_201_066_tests;
#[cfg(test)]
#[path = "tests/versioned_store.rs"]
mod versioned_store_tests;
#[cfg(test)]
#[path = "tests/zc_config_all_optional.rs"]
mod zc_config_all_optional_tests;
#[cfg(test)]
#[path = "tests/zc_config_explain.rs"]
mod zc_config_explain_tests;
#[cfg(test)]
#[path = "tests/zc_debt_report.rs"]
mod zc_debt_report_tests;
#[cfg(test)]
#[path = "tests/zc_setting_registry.rs"]
mod zc_setting_registry_tests;

/// Serializes tests that mutate or read process-global environment-derived
/// paths (`HOME`, `XDG_CONFIG_HOME`, `SUSI_*`). Mutators must hold this lock
/// for the whole env-swap window; readers of `SusiDirs`-derived paths must
/// hold it while resolving so a swapped HOME cannot flip path selection
/// mid-test.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks this process as the `susi-config` service (served by
/// `susi-leaf-services`): it is the canonical writer, so config reads and
/// writes resolve locally instead of calling back into the service.
pub fn enter_service_mode() {
    susi_paths::enter_service_mode();
}
