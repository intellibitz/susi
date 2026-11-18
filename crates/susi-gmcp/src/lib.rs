#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

/// GMCP's own MCP server (wire model, sessions, stdio and Streamable HTTP).
pub mod mcp;
/// The path the typed MCP API has always had: consumers name
/// `susi_gmcp::rmcp::model::*` for the typed form the canonical content
/// model adapts into (`content::CanonicalContentBlock::to_rmcp`). It is
/// GMCP's own implementation in [`mcp`]; no MCP SDK is linked.
pub use self::mcp as rmcp;

pub use susi_abi;

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod catalog;
pub mod content;
pub mod direct_call;
pub mod embed_provider;
pub mod extensions;
pub mod jsonrpc_gate;
pub mod mcp_call_budgets;
pub mod mcp_registry_trust;
pub mod mcp_wrapper;
pub mod protocol;
pub mod requirements_ledger;
pub mod schema_check;
pub mod sdk;
pub mod server;
pub mod stdio;
pub mod tool_registry;
pub mod tool_types;
pub mod tools;

use std::path::Path;

pub use crate::susi_core::plane_bus::tools as plane_tools;

/// GMCP Host: The unified execution entry point for the Meta-Intelligence Substrate.
pub struct GmcpHost;

impl GmcpHost {
    pub fn dispatch(name: &str, arg: &str, workspace: &Path) -> String {
        let val = serde_json::from_str(arg).unwrap_or(serde_json::json!(arg));
        plane_tools::execute_tool(name, &val, workspace).unwrap_or_else(|e| format!("[Error] {e}"))
    }
}

#[cfg(test)]
mod eco_mcp_features;
#[cfg(test)]
#[path = "tests/mcp_bus.rs"]
mod mcp_bus_tests;
#[cfg(test)]
#[path = "tests/protocol.rs"]
mod protocol_tests;
#[cfg(test)]
#[path = "tests/resource_surface.rs"]
mod resource_surface_tests;

#[cfg(test)]
pub(crate) mod test_isolation {
    /// RAII guard holding `ENV_LOCK` while process-wide `HOME`, XDG, and
    /// `SUSI_HOME` point to a private temp directory. Field drop order ensures
    /// env vars restore first, the tempdir deletes second, and `ENV_LOCK`
    /// unlocks last.
    pub(crate) struct IsolatedHomeGuard {
        _env: susi_paths::test_env::EnvGuard,
        _tmp: susi_paths::tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    pub(crate) fn enter_isolated_home() -> IsolatedHomeGuard {
        let lock = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = susi_paths::tempfile::tempdir().expect("create isolated test home");
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", tmp.path());
        env.set("XDG_CONFIG_HOME", tmp.path().join(".config"));
        env.set("XDG_DATA_HOME", tmp.path().join(".local").join("share"));
        env.set("XDG_CACHE_HOME", tmp.path().join(".cache"));
        env.set("SUSI_HOME", tmp.path().join(".susi"));
        IsolatedHomeGuard {
            _env: env,
            _tmp: tmp,
            _lock: lock,
        }
    }

    /// Run a synchronous test inside a fresh, process-env-isolated `SUSI_HOME`/`HOME`
    /// directory so `PlaneBus::global()`, `SusiDirs`, and tool/bus discovery never
    /// touch the user's real `~/.susi` or route requests to a host daemon.
    pub(crate) fn with_isolated_home<R>(f: impl FnOnce(&std::path::Path) -> R) -> R {
        let guard = enter_isolated_home();
        f(guard._tmp.path())
    }
}

#[cfg(test)]
// Test-only assertions in extracted core tools unit suite.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "tests/core_tools.rs"]
mod core_tools_tests;
#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "tests/lib_inline.rs"]
mod inline_tests;
