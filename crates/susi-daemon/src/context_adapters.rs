//! OS-level context adapters for the Universal Context Graph.
//!
//! These adapters are best-effort and platform-gated. They read only public,
//! non-sensitive host state (visible process names, active window title) and
//! never capture keystrokes, passwords, or hidden UI content.

use std::path::Path;
use susi_core::context_graph::ContextGraph;

/// Sample one round of OS context and record it into the global context graph.
/// Linux records the active window (via `xdotool`) and the running process
/// names; Windows and macOS record the running process names through the
/// vendored `sysinfo` facade. What this OS cannot sample is listed by
/// [`unavailable_os_context`]. Returns how many context records were written.
pub fn sample_os_context_once(workspace: Option<&Path>, user: Option<&str>) -> usize {
    #[cfg(target_os = "linux")]
    {
        sample_linux_once(workspace, user)
    }
    #[cfg(not(target_os = "linux"))]
    {
        sample_portable_once(workspace, user)
    }
}

/// `susi context-graph sample-os` / MCP `context_graph_sample_os`: sample
/// once and report how many records were written, the OS, and the sources
/// this OS cannot sample (with the reason).
pub fn sample_os_report(workspace: Option<&Path>, user: Option<&str>) -> serde_json::Value {
    let count = sample_os_context_once(workspace, user);
    let unavailable: serde_json::Map<String, serde_json::Value> = unavailable_os_context()
        .into_iter()
        .map(|(source, reason)| (source.to_string(), reason.into()))
        .collect();
    serde_json::json!({
        "sampled": count,
        "os": std::env::consts::OS,
        "unavailable": unavailable,
    })
}

/// OS context adapters this platform cannot run, with the reason — so a
/// sample that records fewer sources says why instead of looking empty.
pub fn unavailable_os_context() -> Vec<(&'static str, String)> {
    if cfg!(target_os = "linux") {
        Vec::new()
    } else {
        vec![(
            "active_window",
            format!(
                "active-window sampling is Linux-only (xdotool); not sampled on {}",
                std::env::consts::OS
            ),
        )]
    }
}

/// Windows/macOS: running process names (image basenames) through `sysinfo`,
/// recorded as `<os>_proc` (`windows_proc`, `macos_proc`).
#[cfg(not(target_os = "linux"))]
fn sample_portable_once(workspace: Option<&Path>, user: Option<&str>) -> usize {
    let processes = portable_process_names();
    if processes.is_empty() {
        return 0;
    }
    let payload = serde_json::json!({ "processes": processes });
    ContextGraph::global().record_external_context(
        &format!("{}_proc", std::env::consts::OS),
        &format!("{} running processes", processes.len()),
        &payload,
        workspace,
        user,
    );
    1
}

/// Sorted, de-duplicated process names, capped like the Linux sampler.
#[cfg(not(target_os = "linux"))]
fn portable_process_names() -> Vec<String> {
    let mut names: Vec<String> = susi_core::susi_config::process_probe::processes(false)
        .into_iter()
        .map(|p| {
            p.exe
                .as_deref()
                .and_then(|exe| exe.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(p.name)
        })
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    // Cap to keep payloads small.
    names.truncate(128);
    names
}

#[cfg(target_os = "linux")]
fn sample_linux_once(workspace: Option<&Path>, user: Option<&str>) -> usize {
    let mut count = 0;
    if let Ok(window) = active_window_title() {
        let payload = serde_json::json!({ "active_window": window });
        ContextGraph::global().record_external_context(
            "linux_active_window",
            &format!("active window: {window}"),
            &payload,
            workspace,
            user,
        );
        count += 1;
    }
    if let Ok(processes) = top_processes() {
        let payload = serde_json::json!({ "processes": processes });
        ContextGraph::global().record_external_context(
            "linux_proc",
            &format!("{} running processes", processes.len()),
            &payload,
            workspace,
            user,
        );
        count += 1;
    }
    count
}

#[cfg(target_os = "linux")]
fn active_window_title() -> std::io::Result<String> {
    let output = std::process::Command::new("xdotool")
        .args(["getactivewindow", "getwindowname"])
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("xdotool failed"));
    }
    let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if title.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "empty window title",
        ));
    }
    Ok(title)
}

#[cfg(target_os = "linux")]
fn top_processes() -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
        {
            let cmdline = entry.path().join("cmdline");
            if let Ok(text) = std::fs::read_to_string(&cmdline) {
                let text = text.replace('\0', " ");
                let first = text.split_whitespace().next().unwrap_or(&text);
                let base = std::path::Path::new(first)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(first)
                    .to_string();
                if !base.is_empty() && !names.contains(&base) {
                    names.push(base);
                }
            }
        }
    }
    names.sort();
    if names.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no processes found",
        ));
    }
    // Cap to keep payloads small.
    names.truncate(128);
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn sample_os_context_records_processes_on_non_linux() {
        assert!(!portable_process_names().is_empty());
        assert_eq!(sample_os_context_once(None, None), 1);
        let gaps = unavailable_os_context();
        assert!(gaps.iter().any(|(source, _)| *source == "active_window"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sample_os_context_records_without_panicking() {
        // The Linux adapter is best-effort; this just verifies it does not panic.
        let _ = sample_os_context_once(None, None);
    }

    #[test]
    fn sample_os_report_includes_sampled_count_os_and_unavailable_map() {
        let report = sample_os_report(Some(Path::new(".")), Some("unit-tester"));
        assert_eq!(report["os"].as_str(), Some(std::env::consts::OS));
        assert!(report["sampled"].as_u64().is_some());
        assert!(report["unavailable"].is_object());
    }

    #[test]
    fn unavailable_os_context_matches_platform_capabilities() {
        let gaps = unavailable_os_context();
        if cfg!(target_os = "linux") {
            assert!(gaps.is_empty());
        } else {
            assert_eq!(gaps.len(), 1);
            assert_eq!(gaps[0].0, "active_window");
            assert!(gaps[0].1.contains(std::env::consts::OS));
        }
    }

    #[test]
    fn sample_os_context_once_accepts_workspace_and_user_scope() {
        let tmp = std::env::temp_dir();
        let count = sample_os_context_once(Some(tmp.as_path()), Some("scoped-user"));
        if !cfg!(target_os = "linux") {
            assert_eq!(count, 1);
        }
    }
}
