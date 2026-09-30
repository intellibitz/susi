//! `EnvGuard` scrubs and restores the instance-selecting env vars.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::ffi::OsStr;
use susi_paths::test_env::EnvGuard;

#[test]
fn isolated_removes_instance_vars_and_restores_them() {
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
