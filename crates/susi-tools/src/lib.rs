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

    let parent = susi_paths::tempfile::tempdir().unwrap();
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

#[cfg(test)]
mod surface_tests {
    use super::*;

    #[test]
    fn crate_root_re_exports_resolve_to_their_defining_modules() {
        // Each root re-export must be the same type as its module path, so
        // downstream crates (gmcp, gawd, daemon) can depend on the root.
        fn same<T: 'static, U: 'static>() -> bool {
            std::any::TypeId::of::<T>() == std::any::TypeId::of::<U>()
        }
        assert!(same::<GlobalMcpEntry, config::GlobalMcpEntry>());
        assert!(same::<McpConfig, config::McpConfig>());
        assert!(same::<McpServerConfig, config::McpServerConfig>());
        assert!(same::<HardwareSnapshot, hooks::HardwareSnapshot>());
        assert!(same::<ToolRegistry, registry::ToolRegistry>());
        assert!(same::<MetaCategory, types::MetaCategory>());
        assert!(same::<GmcpClient, client::GmcpClient>());
    }

    #[test]
    fn meta_category_variants_are_distinct_copy_values() {
        let all = [
            MetaCategory::SystemPrimitive,
            MetaCategory::WorkspaceIo,
            MetaCategory::McpProxy,
            MetaCategory::WasmReflex,
            MetaCategory::IntelligenceBridge,
            MetaCategory::CodingSpecialist,
            MetaCategory::AssistantSpecialist,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b, "{a:?} vs {b:?}");
            }
        }
        let copied = all[0];
        assert_eq!(copied, MetaCategory::SystemPrimitive);
    }

    #[test]
    fn tool_registry_global_is_a_singleton_wired_through_hooks() {
        // `global()` bootstraps through `hooks()`; in the unit-test binary
        // that is the inert fallback, so this never touches gawd/gmcp.
        assert_eq!(hooks::hooks().engine_version(), "unknown");
        assert!(std::ptr::eq(ToolRegistry::global(), ToolRegistry::global()));
    }

    #[test]
    fn meta_tool_and_leading_mcp_types_resolve_through_crate_root() {
        fn same<T: 'static, U: 'static>() -> bool {
            std::any::TypeId::of::<T>() == std::any::TypeId::of::<U>()
        }
        assert!(same::<
            LeadingMcpDefinition,
            leading_mcp::LeadingMcpDefinition,
        >());
        assert!(same::<LeadingMcpManager, leading_mcp::LeadingMcpManager>());
        assert!(same::<LeadingMcpOverride, leading_mcp::LeadingMcpOverride>());
        assert!(same::<McpTool, types::McpTool>());
        assert!(same::<MetaTool, types::MetaTool>());
        assert!(same::<MetaToolHandler, types::MetaToolHandler>());
        assert_eq!(format!("{:?}", MetaCategory::WasmReflex), "WasmReflex");
    }

    #[test]
    fn re_exported_error_and_config_crates_resolve_through_susi_tools_root() {
        let err = susi_error::EaiError::internal("tools-root-probe");
        assert!(err.to_string().contains("tools-root-probe"));
        assert!(!susi_config::SusiConfig::default().settings.is_empty());
    }
}
