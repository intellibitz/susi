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
}
