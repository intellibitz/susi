#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `.mcp.json` is read by every Claude Code session that opens this
//! repository: a laptop's, a teammate's, and a cloud session's disposable
//! container (docs/cloud-sessions.md). It once named one laptop's build —
//! `F:/src/github.com/intellibitz/susi/target/debug/susi.exe`, with
//! `SUSI_HOME=C:/Users/user/.susi` — so every cloud session started with a
//! failed `susi-harness` server and a prompt to have the operator fix it on
//! their machine. A checked-in server names the clone's own build, relative
//! to the clone, and no host's home.

fn mcp_servers() -> serde_json::Map<String, serde_json::Value> {
    let text = std::fs::read_to_string(".mcp.json").unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    json["mcpServers"].as_object().unwrap().clone()
}

/// A path only one machine has: absolute (`/…`, `\…`, `~…`, a drive letter)
/// or under a user's home directory.
fn names_a_machine_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let normalized = value.replace('\\', "/");
    drive
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with('~')
        || normalized.contains("/Users/")
        || normalized.contains("/home/")
}

#[test]
fn checked_in_mcp_servers_name_no_machine_path() {
    let servers = mcp_servers();
    assert!(!servers.is_empty(), ".mcp.json declares no server");
    for (name, server) in &servers {
        let mut values: Vec<&str> = Vec::new();
        values.extend(server["command"].as_str());
        if let Some(args) = server["args"].as_array() {
            values.extend(args.iter().filter_map(|a| a.as_str()));
        }
        if let Some(env) = server["env"].as_object() {
            values.extend(env.values().filter_map(|v| v.as_str()));
        }
        for value in values {
            assert!(
                !names_a_machine_path(value),
                "{name} names one machine's path {value:?}: a cloud session (and every other clone) cannot reach it"
            );
        }
    }
}

#[test]
fn susi_harness_runs_the_clones_own_build_with_the_default_home() {
    let servers = mcp_servers();
    let server = &servers["susi-harness"];
    assert_eq!(server["command"], "target/debug/susi");
    assert_eq!(server["args"], serde_json::json!(["mcp"]));
    assert!(
        server["env"].get("SUSI_HOME").is_none(),
        "SUSI_HOME is the host's to set; ~/.susi is the default everywhere"
    );
}

#[test]
fn a_machine_path_is_told_from_a_clone_relative_one() {
    for path in [
        "F:/src/github.com/intellibitz/susi/target/debug/susi.exe",
        "C:/Users/user/.susi",
        "C:\\Users\\user\\.susi",
        "/home/user/susi/target/debug/susi",
        "~/.susi",
        "\\\\server\\share\\susi.exe",
    ] {
        assert!(names_a_machine_path(path), "{path}");
    }
    for path in ["target/debug/susi", "mcp", "scripts/susi-mcp.sh"] {
        assert!(!names_a_machine_path(path), "{path}");
    }
}
