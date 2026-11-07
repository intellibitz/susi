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
pub mod credential;
pub mod desired_state;
pub mod explain;
pub mod extensions;
pub mod file_lock;
pub mod host_metrics;
mod json_util;
pub mod key_rotation;
pub mod key_scope;
pub mod offline_bundle;
pub mod process_probe;
pub mod rekey_schedule;
pub mod secret_ref;
pub mod selfheal;
pub mod setting_registry;
pub mod setup_workflow;
pub mod state_migration;
mod types;
pub mod validate;
pub mod versioned_store;
#[cfg(windows)]
pub mod windows_environment;
#[cfg(windows)]
pub mod windows_model_identity;
pub mod zc_bare_susi;
pub mod zc_debt_ratchet;
pub mod zc_debt_report;
pub mod zc_path;
pub mod zc_qualification;
pub mod zc_susi_fix;

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
        let resp = loopback::request_with_fallback(
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
        loopback::request_with_fallback(
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

    #[cfg(test)]
    mod tests {
        use super::super::env_test_lock;
        use super::SusiConfig;

        /// Sets `vars` for the closure's duration and restores the previous
        /// values afterwards, even if an assertion fails.
        fn with_env<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
            struct Restore(Vec<(String, Option<std::ffi::OsString>)>);
            impl Drop for Restore {
                fn drop(&mut self) {
                    for (key, old) in self.0.drain(..) {
                        match old {
                            Some(v) => std::env::set_var(&key, v),
                            None => std::env::remove_var(&key),
                        }
                    }
                }
            }
            let _restore = Restore(
                vars.iter()
                    .map(|(k, _)| ((*k).to_string(), std::env::var_os(k)))
                    .collect(),
            );
            for (key, value) in vars {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            f()
        }

        #[test]
        fn port_honours_the_override_and_falls_back_to_the_contract_port() {
            let _guard = env_test_lock();
            with_env(&[("SUSI_CONFIG_PORT", Some(" 28082 "))], || {
                assert_eq!(super::port(), 28082);
            });
            with_env(&[("SUSI_CONFIG_PORT", Some("not-a-port"))], || {
                assert_eq!(
                    super::port(),
                    susi_paths::ports::effective(susi_paths::ports::CONFIG_SERVICE)
                );
            });
            with_env(&[("SUSI_CONFIG_PORT", None)], || {
                assert_eq!(
                    super::port(),
                    susi_paths::ports::effective(susi_paths::ports::CONFIG_SERVICE)
                );
            });
        }

        #[test]
        fn explicit_local_env_config_keeps_global_config_off_the_service() {
            let _guard = env_test_lock();
            // Point the port at something nothing listens on: if the local-only
            // guard ever regressed, the calls below would fail rather than reach
            // a real substrate on this host.
            with_env(
                &[("SUSI_XDG", Some("1")), ("SUSI_CONFIG_PORT", Some("1"))],
                || {
                    assert!(super::local_only());
                    let started = std::time::Instant::now();
                    assert!(super::get_global().is_none());
                    assert!(!super::save_global(&SusiConfig::default()));
                    assert!(
                        started.elapsed() < std::time::Duration::from_millis(150),
                        "local-only resolution must not attempt network I/O"
                    );
                },
            );
        }

        #[test]
        fn enter_service_mode_forces_local_only_resolution_without_network_io() {
            let _guard = env_test_lock();
            with_env(
                &[
                    ("SUSI_XDG", None),
                    ("XDG_CONFIG_HOME", None),
                    ("XDG_DATA_HOME", None),
                    ("XDG_CACHE_HOME", None),
                    ("SUSI_CONFIG_PORT", Some("1")),
                ],
                || {
                    super::super::enter_service_mode();
                    assert!(super::local_only());
                    let started = std::time::Instant::now();
                    assert!(super::get_global().is_none());
                    assert!(!super::save_global(&SusiConfig::default()));
                    assert!(
                        started.elapsed() < std::time::Duration::from_millis(150),
                        "service-mode resolution must not attempt network I/O"
                    );
                },
            );
        }

        #[test]
        fn xdg_config_home_override_triggers_local_only_resolution() {
            let _guard = env_test_lock();
            let temp_xdg = std::env::temp_dir().join("susi-config-xdg-local-only-test");
            let temp_xdg_str = temp_xdg.to_string_lossy().into_owned();
            with_env(
                &[
                    ("SUSI_XDG", None),
                    ("XDG_CONFIG_HOME", Some(&temp_xdg_str)),
                    ("XDG_DATA_HOME", None),
                    ("XDG_CACHE_HOME", None),
                    ("SUSI_CONFIG_PORT", Some("1")),
                ],
                || {
                    assert!(susi_paths::loopback::local_env_override());
                    assert!(super::local_only());
                    let started = std::time::Instant::now();
                    assert!(super::get_global().is_none());
                    assert!(!super::save_global(&SusiConfig::default()));
                    assert!(
                        started.elapsed() < std::time::Duration::from_millis(150),
                        "XDG_CONFIG_HOME override must short-circuit without network I/O"
                    );
                },
            );
        }

        #[test]
        fn xdg_data_home_override_triggers_local_only_resolution() {
            let _guard = env_test_lock();
            let temp_data = std::env::temp_dir().join("susi-config-xdg-data-local-only-test");
            let temp_data_str = temp_data.to_string_lossy().into_owned();
            with_env(
                &[
                    ("SUSI_XDG", None),
                    ("XDG_CONFIG_HOME", None),
                    ("XDG_DATA_HOME", Some(&temp_data_str)),
                    ("XDG_CACHE_HOME", None),
                    ("SUSI_CONFIG_PORT", Some("1")),
                ],
                || {
                    assert!(susi_paths::loopback::local_env_override());
                    assert!(super::local_only());
                },
            );
        }
    }
}

pub use cloud_env::{cloud_env_overlay, env_or_cloud_env};
pub use config::{redact_credentials, SusiConfig};
pub use credential::CredentialRef;
pub use desired_state::{
    parse_desired_state, round_trip_desired_state, validate_desired_state, DesiredRef,
    DesiredState, DESIRED_STATE_SCHEMA,
};
pub use explain::{explain_key, explain_key_global, ConfigLayer, ConfigOrigin, ExplainedSetting};
pub use json_util::{
    atomic_replace_file, atomic_replace_unsynced, atomic_write_bytes, atomic_write_json_pretty,
    clear_json_override, confined_workspace_join, create_private_dir, file_node,
    install_private_file, load_or_create_secret, merge_missing_json_defaults,
    merge_missing_registry_defaults, remove_file_if_present, write_json_override, DynamicRegistry,
    DynamicValue, ModelTier, ProviderType, StringRegistry,
};
pub use key_scope::{resolve_key_ref, KeyRef, KeyScope, ScopedSecret};
pub use rekey_schedule::{evaluate_rekey_policy, RekeyPolicy, RekeyReason, RekeyScheduleDecision};
pub use secret_ref::{redact_for_export, ResolvedSecret, ScopeBoundary, SecretRef, SecretVault};
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
#[path = "tests/key_rotation.rs"]
mod key_rotation_tests;
#[cfg(test)]
#[path = "tests/vc_201_076_mastery.rs"]
mod vc_201_076_mastery_tests;

#[cfg(test)]
#[path = "tests/key_scope.rs"]
mod key_scope_tests;
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
#[path = "tests/state_migration.rs"]
mod state_migration_tests;
#[cfg(test)]
#[path = "tests/vc_201_061_mastery.rs"]
mod vc_201_061_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_062_mastery.rs"]
mod vc_201_062_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_065_mastery.rs"]
mod vc_201_065_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_065.rs"]
mod vc_201_065_tests;
#[cfg(test)]
#[path = "tests/vc_201_066_mastery.rs"]
mod vc_201_066_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_066.rs"]
mod vc_201_066_tests;
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
#[path = "tests/vc_201_069_mastery.rs"]
mod vc_201_069_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_077_mastery.rs"]
mod vc_201_077_mastery_tests;
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
