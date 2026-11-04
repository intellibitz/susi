//! `EnvGuard` scrubs and restores the instance-selecting env vars.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::ffi::OsStr;
use std::sync::Mutex;
use susi_paths::test_env::EnvGuard;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn isolated_removes_instance_vars_and_restores_them() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut outer = EnvGuard::isolated();
    outer.set("SUSI_HOME", "/tmp/susi-env-guard-a");
    outer.set("SUSI_PORT_OFFSET", "100");
    {
        let mut inner = EnvGuard::isolated();
        assert!(std::env::var_os("SUSI_HOME").is_none());
        assert!(std::env::var_os("SUSI_PORT_OFFSET").is_none());
        inner.set("SUSI_HOME", "/tmp/susi-env-guard-b");
    }
    assert_eq!(
        std::env::var_os("SUSI_HOME").as_deref(),
        Some(OsStr::new("/tmp/susi-env-guard-a"))
    );
    assert_eq!(
        std::env::var_os("SUSI_PORT_OFFSET").as_deref(),
        Some(OsStr::new("100"))
    );
    drop(outer);
}

#[test]
fn env_guard_remove_restores_previous_value_on_drop() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut outer = EnvGuard::isolated();
    outer.set("SUSI_TEST_CUSTOM_VAR", "initial-val");
    {
        let mut inner = EnvGuard::isolated();
        inner.remove("SUSI_TEST_CUSTOM_VAR");
        assert!(std::env::var_os("SUSI_TEST_CUSTOM_VAR").is_none());
    }
    assert_eq!(
        std::env::var_os("SUSI_TEST_CUSTOM_VAR").as_deref(),
        Some(OsStr::new("initial-val"))
    );
}

#[test]
fn env_guard_set_overwrites_multiple_times_and_restores_initial_state() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut outer = EnvGuard::isolated();
    outer.remove("SUSI_TEST_MULTI_SET");
    {
        let mut inner = EnvGuard::isolated();
        inner.set("SUSI_TEST_MULTI_SET", "v1");
        inner.set("SUSI_TEST_MULTI_SET", "v2");
        assert_eq!(
            std::env::var_os("SUSI_TEST_MULTI_SET").as_deref(),
            Some(OsStr::new("v2"))
        );
    }
    assert!(std::env::var_os("SUSI_TEST_MULTI_SET").is_none());
}

#[test]
fn env_guard_isolated_scrubs_susi_home_and_port_offset_independently() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = EnvGuard::isolated();
    assert!(std::env::var_os("SUSI_HOME").is_none());
    assert!(std::env::var_os("SUSI_PORT_OFFSET").is_none());
    drop(env);
}
