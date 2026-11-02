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

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

// One compiled native-service client; Wasmer remains isolated in the service.
pub use susi_native_client as susi_native;

pub use susi_core;

pub mod plane_handler;

pub mod client;
pub mod config;
pub mod hooks;
pub mod leading_mcp;
pub mod registry;
pub mod types;

pub use client::GmcpClient;
mod mcp_budget;
pub use config::{GlobalMcpEntry, McpConfig, McpServerConfig};
pub use hooks::{EngineHooks, HardwareSnapshot};
pub use leading_mcp::{LeadingMcpDefinition, LeadingMcpManager, LeadingMcpOverride};
pub use registry::ToolRegistry;
pub use types::{McpTool, MetaCategory, MetaTool, MetaToolHandler, SusiTool};

/// Run the calling test with a private configuration root from process birth.
/// Only the child executes the test body. Its home outlives its MAC singleton.
#[cfg(test)]
fn private_config_fixture() -> Option<std::path::PathBuf> {
    const CHILD: &str = "SUSI_TOOLS_CONFIG_TEST_CHILD";
    let thread = std::thread::current();
    let test = thread.name().expect("libtest names the active test thread");
    let cwd = std::env::current_dir().unwrap();
    if std::env::var(CHILD).as_deref() == Ok(test)
        && std::fs::read_to_string(cwd.join(".config-test"))
            .as_deref()
            .ok()
            == Some(test)
    {
        let state = cwd.join("state");
        assert_eq!(std::env::var_os("SUSI_HOME").unwrap(), state.as_os_str());
        return Some(state);
    }

    let parent = tempfile::tempdir().unwrap();
    let root = parent.path();
    let state = root.join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::write(root.join(".config-test"), test).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.env_clear();
    for key in ["SystemRoot", "WINDIR", "PATH", "ComSpec"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .current_dir(root)
        .env(CHILD, test)
        .env("SUSI_HOME", &state);
    let output = susi_paths::test_env::isolate_home(&mut command, root)
        .args(["--exact", test, "--test-threads=1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parent.close().unwrap();
    None
}
