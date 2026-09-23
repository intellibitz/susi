//! Path/SSRF/argv guards and external-agent task helpers for CoreTools.

use crate::susi_core::plane_bus::agents;
use crate::susi_error::{EaiError, EaiResult};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(super) fn external_agent_control(
    arg: &serde_json::Value,
    workspace: &Path,
    action: &str,
) -> EaiResult<String> {
    let id = arg
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| EaiError::protocol("task_id is required"))?;
    for kind in ["execution", "framework"] {
        if let Ok(v) = agents::external_control(workspace, kind, id, action, arg) {
            if action == "logs" {
                if let Some(text) = v.get("text").and_then(|x| x.as_str()) {
                    return Ok(text.to_string());
                }
            }
            return serde_json::to_string(&v).map_err(|e| EaiError::protocol(e.to_string()));
        }
    }
    Err(EaiError::protocol(format!("unknown task_id {id}")))
}

/// Ensure path is normalized and contained within workspace
pub(super) fn secure_path(workspace: &Path, user_path: &str) -> EaiResult<PathBuf> {
    let user_path = user_path.trim().trim_matches('"').trim_matches('\'');
    let path = PathBuf::from(user_path);

    if path.is_absolute() {
        return Err(EaiError::filesystem("Absolute paths not allowed"));
    }

    for component in path.components() {
        if let Component::ParentDir = component {
            return Err(EaiError::filesystem(
                "Parent directory traversal not allowed",
            ));
        }
    }

    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|e| EaiError::filesystem(format!("Workspace error: {}", e)))?;

    let full_path = workspace.join(&path);

    let parent = full_path.parent().unwrap_or(workspace);
    let canonical_parent = parent
        .canonicalize()
        .map_err(|_| EaiError::filesystem("Invalid path hierarchy (doesn't exist)"))?;

    if !canonical_parent.starts_with(&canonical_workspace) {
        return Err(EaiError::filesystem(format!(
            "Path escape attempt: {}",
            user_path
        )));
    }

    let leaf_name = full_path
        .file_name()
        .ok_or_else(|| EaiError::filesystem(format!("Path has no file name: {}", user_path)))?;
    let candidate = canonical_parent.join(leaf_name);

    if candidate.exists() || candidate.symlink_metadata().is_ok() {
        let meta = std::fs::symlink_metadata(&candidate)
            .map_err(|e| EaiError::filesystem(format!("Cannot stat path {}: {}", user_path, e)))?;
        if meta.file_type().is_symlink() {
            return Err(EaiError::filesystem(format!(
                "Symlink targets not allowed: {}",
                user_path
            )));
        }
        let resolved = candidate.canonicalize().map_err(|e| {
            EaiError::filesystem(format!("Cannot resolve path {}: {}", user_path, e))
        })?;
        if !resolved.starts_with(&canonical_workspace) {
            return Err(EaiError::filesystem(format!(
                "Path escape attempt: {}",
                user_path
            )));
        }
        return Ok(resolved);
    }

    Ok(candidate)
}

pub(super) fn read_file_nofollow(path: &Path) -> EaiResult<String> {
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options
            .open(path)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        Ok(content)
    }
    #[cfg(not(unix))]
    {
        fs::read_to_string(path).map_err(|e| EaiError::filesystem(e.to_string()))
    }
}

pub(super) fn confine_exec_argv(workspace: &Path, args: &[String]) -> EaiResult<()> {
    for arg in args.iter().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        let p = Path::new(arg);
        if p.is_absolute() {
            return Err(EaiError::filesystem(format!(
                "Absolute paths not allowed in exec_command: {arg}"
            )));
        }
        if arg.contains("..") {
            return Err(EaiError::filesystem(format!(
                "Parent directory traversal not allowed in exec_command: {arg}"
            )));
        }
        if arg.contains('/') || arg.contains('\\') {
            let _ = secure_path(workspace, arg)?;
        }
    }
    Ok(())
}

#[cfg_attr(not(feature = "tools-rich"), allow(dead_code))]
pub(super) fn is_blocked_ssrf_target(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ssrf_target(std::net::IpAddr::V4(mapped));
            }
            let segments = v6.segments();
            let is_unique_local = (segments[0] & 0xfe00) == 0xfc00;
            let is_unicast_link_local = (segments[0] & 0xffc0) == 0xfe80;
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_unique_local
                || is_unicast_link_local
        }
    }
}

#[cfg_attr(not(feature = "tools-rich"), allow(dead_code))]
pub(super) fn secure_external_url(raw_url: &str) -> EaiResult<url::Url> {
    let parsed =
        url::Url::parse(raw_url).map_err(|e| EaiError::protocol(format!("Invalid URL: {}", e)))?;

    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(EaiError::protocol(format!(
            "SSRF BLOCK: scheme '{}' not allowed (only http/https)",
            parsed.scheme()
        )));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| EaiError::protocol("URL has no host"))?;
    let port = parsed.port_or_known_default().unwrap_or(80);

    use std::net::ToSocketAddrs;
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| EaiError::protocol(format!("SSRF BLOCK: failed to resolve host: {}", e)))?;

    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        if is_blocked_ssrf_target(addr.ip()) {
            return Err(EaiError::protocol(format!(
                "SSRF BLOCK: '{}' resolves to a disallowed internal address ({})",
                host,
                addr.ip()
            )));
        }
    }
    if !resolved_any {
        return Err(EaiError::protocol(
            "SSRF BLOCK: host resolved to no addresses",
        ));
    }

    Ok(parsed)
}

pub(super) fn tool_string_arg(arg: &serde_json::Value, keys: &[&str]) -> EaiResult<String> {
    if let Some(s) = arg.as_str() {
        let s = s.trim();
        if !s.is_empty() {
            return Ok(s.to_string());
        }
    }
    if let Some(obj) = arg.as_object() {
        for key in keys {
            if let Some(s) = obj.get(*key).and_then(|v| v.as_str()) {
                let s = s.trim();
                if !s.is_empty() {
                    return Ok(s.to_string());
                }
            }
        }
    }
    Err(EaiError::protocol(format!(
        "Invalid argument type (expected string or object with one of: {})",
        keys.join(", ")
    )))
}

/// `os_ps` backend: walk /proc, return `pid, comm, VmRSS kB` rows sorted by
/// memory descending, capped at `limit`. Linux-only — other platforms get a
/// clear unsupported error rather than an empty table.
pub(super) fn os_ps_proc(limit: usize, filter: &str) -> EaiResult<String> {
    if !cfg!(target_os = "linux") {
        return Err(EaiError::protocol("os_ps requires /proc (Linux)"));
    }
    let mut rows: Vec<(u32, String, u64)> = Vec::new();
    let entries =
        fs::read_dir("/proc").map_err(|e| EaiError::filesystem(format!("read /proc: {e}")))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let dir = entry.path();
        let comm = fs::read_to_string(dir.join("comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !filter.is_empty() && !comm.contains(filter) {
            continue;
        }
        let rss_kb = fs::read_to_string(dir.join("status"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            })
            .unwrap_or(0);
        rows.push((pid, comm, rss_kb));
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.2));
    rows.truncate(limit);
    let mut out = String::from("PID\tNAME\tRSS_KB\n");
    for (pid, comm, rss) in rows {
        out.push_str(&format!("{pid}\t{comm}\t{rss}\n"));
    }
    Ok(out)
}

/// `os_sysinfo` backend: kernel, uptime, loadavg, meminfo, cpu count.
/// Linux-only for the same reason as `os_ps_proc`.
pub(super) fn os_sysinfo_proc() -> EaiResult<String> {
    if !cfg!(target_os = "linux") {
        return Err(EaiError::protocol("os_sysinfo requires /proc (Linux)"));
    }
    let read = |p: &str| -> String {
        fs::read_to_string(p)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "?".into())
    };
    let kernel = read("/proc/sys/kernel/ostype");
    let release = read("/proc/sys/kernel/osrelease");
    let uptime_secs: f64 = read("/proc/uptime")
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let loadavg = read("/proc/loadavg");
    let meminfo = read("/proc/meminfo");
    let mem_line = |key: &str| -> String {
        meminfo
            .lines()
            .find(|l| l.starts_with(key))
            .unwrap_or("?")
            .to_string()
    };
    let cpus = read("/proc/cpuinfo")
        .lines()
        .filter(|l| l.starts_with("processor"))
        .count();
    Ok(format!(
        "kernel: {kernel} {release}\n\
         uptime: {:.0}s\n\
         loadavg: {loadavg}\n\
         {}\n\
         {}\n\
         cpus: {cpus}",
        uptime_secs,
        mem_line("MemTotal:"),
        mem_line("MemAvailable:")
    ))
}
