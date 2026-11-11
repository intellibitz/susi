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
    // Tasks live in the workspace the CLI ran them in; the socket takes an
    // optional, MAC-policy-admitted `workspace` (cancel and send write).
    let write = !matches!(action, "status" | "logs");
    let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, write)?;
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
        // A path can ride inside an option (`--output=/etc/x`, `-o/etc/x`,
        // `-C..`); check the option's value exactly like a positional path.
        let candidate = if let Some(long) = arg.strip_prefix("--") {
            match long.split_once('=') {
                Some((_, value)) => value,
                None => continue,
            }
        } else if let Some(short) = arg.strip_prefix('-') {
            match short.char_indices().nth(1) {
                Some((i, _)) => &short[i..],
                None => continue,
            }
        } else {
            arg.as_str()
        };
        confine_exec_path(workspace, arg, candidate)?;
    }
    Ok(())
}

fn confine_exec_path(workspace: &Path, arg: &str, candidate: &str) -> EaiResult<()> {
    if candidate.is_empty() {
        return Ok(());
    }
    if Path::new(candidate).is_absolute() || candidate.starts_with('~') {
        return Err(EaiError::filesystem(format!(
            "Absolute paths not allowed in exec_command: {arg}"
        )));
    }
    if candidate.contains("..") {
        return Err(EaiError::filesystem(format!(
            "Parent directory traversal not allowed in exec_command: {arg}"
        )));
    }
    if candidate.contains('/') || candidate.contains('\\') {
        let _ = secure_path(workspace, candidate)?;
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
pub(super) fn secure_external_url(raw_url: &str) -> EaiResult<susi_paths::weburl::WebUrl> {
    // Only http(s) parses (SSRF: no other scheme is fetched).
    let parsed = susi_paths::weburl::WebUrl::parse(raw_url)
        .map_err(|e| EaiError::protocol(format!("Invalid URL: {}", e)))?;

    let (host, port) = (parsed.host_str(), parsed.port_or_known_default());

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

/// `os_ps` backend: list processes (threads excluded) through the vendored
/// sysinfo facade on every OS and return `pid, name, RSS kB` rows sorted by
/// memory descending, capped at `limit`. The name column is `comm` on Linux
/// and the image name on Windows and macOS. A host that reports no processes
/// gets an explicit error naming the OS rather than an empty table.
pub(super) fn os_ps_proc(limit: usize, filter: &str) -> EaiResult<String> {
    // Every OS through the vendored sysinfo facade (D-OS-1); the name column
    // is `comm` on Linux and the image name on Windows and macOS.
    use crate::susi_config::process_probe;
    let all = process_probe::processes(false);
    if all.is_empty() {
        return Err(EaiError::protocol(format!(
            "os_ps: this OS ({}) reports no processes",
            std::env::consts::OS
        )));
    }
    let mut rows: Vec<(u32, String, u64)> = all
        .into_iter()
        .filter(|p| filter.is_empty() || p.name.contains(filter))
        .map(|p| (p.pid, p.name, p.rss_kb))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.2));
    rows.truncate(limit);
    let mut out = String::from("PID\tNAME\tRSS_KB\n");
    for (pid, comm, rss) in rows {
        out.push_str(&format!("{pid}\t{comm}\t{rss}\n"));
    }
    Ok(out)
}

/// `os_sysinfo` backend: kernel, uptime, loadavg, meminfo, cpu count, read
/// through the vendored sysinfo facade on every OS. The first six lines keep
/// their historical order; `os:` and `cpu_util:` are appended after `cpus:`.
/// On Linux `loadavg:` stays the raw `/proc/loadavg` record; elsewhere it is
/// the three averages. `MemTotal:`/`MemAvailable:` are rendered as
/// `<key>: <n> kB` (the meminfo value without its column padding). `cpus:` is
/// the logical CPU count available to this process (`available_parallelism`,
/// so affinity and cgroup limits apply, and it can be below the number of
/// processors the OS reports, e.g. `/proc/cpuinfo` on Linux). A metric the
/// OS does not keep reads
/// `unavailable ...` instead of a made-up zero.
pub(super) fn os_sysinfo_proc() -> EaiResult<String> {
    // Every OS through the vendored sysinfo facade (D-OS-1). A metric the OS
    // does not keep is named as unavailable, never printed as a zero.
    use crate::susi_config::host_metrics;
    let kernel = host_metrics::kernel_name();
    let release = host_metrics::kernel_version().unwrap_or_else(|| "?".into());
    let os = host_metrics::long_os_version().unwrap_or_else(|| "?".into());
    let uptime = host_metrics::uptime_secs()
        .map_or_else(|| "unavailable on this OS".to_string(), |s| format!("{s}s"));
    // Linux keeps the full /proc/loadavg record (averages, runnable/total
    // tasks, last pid) so the line stays byte-compatible with the
    // pre-sysinfo output; other OSes print the three averages.
    let raw_loadavg = if cfg!(target_os = "linux") {
        fs::read_to_string("/proc/loadavg")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let loadavg = match (raw_loadavg, host_metrics::load_average()) {
        (Some(raw), _) => raw,
        (None, Some([one, five, fifteen])) => format!("{one:.2} {five:.2} {fifteen:.2}"),
        (None, None) => format!(
            "unavailable ({})",
            host_metrics::load_average_unavailable().unwrap_or("not reported")
        ),
    };
    let (mem_total, mem_avail) = match host_metrics::memory() {
        Some(mem) => (
            format!("MemTotal: {} kB", mem.total_bytes / 1024),
            format!("MemAvailable: {} kB", mem.available_bytes / 1024),
        ),
        None => (
            "MemTotal: unavailable on this OS".to_string(),
            "MemAvailable: unavailable on this OS".to_string(),
        ),
    };
    let cpu_util = host_metrics::cpu_util_pct().map_or_else(
        || "unavailable on this OS".to_string(),
        |p| format!("{p:.1}%"),
    );
    let cpus = host_metrics::cpu_count();
    Ok(format!(
        "kernel: {kernel} {release}\n\
         uptime: {uptime}\n\
         loadavg: {loadavg}\n\
         {mem_total}\n\
         {mem_avail}\n\
         cpus: {cpus}\n\
         os: {os}\n\
         cpu_util: {cpu_util}"
    ))
}

#[cfg(test)]
mod confine_tests {
    use super::confine_exec_argv;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn paths_hidden_in_options_are_confined() {
        let ws = std::env::temp_dir();
        for escape in [
            ["git", "--git-dir=/root/.ssh"],
            ["tool", "--output=/etc/cron.d/x"],
            ["cc", "-o/etc/passwd"],
            ["make", "-C.."],
            ["tool", "--cfg=../../secret"],
            ["tool", "--home=~/.aws"],
        ] {
            assert!(
                confine_exec_argv(&ws, &argv(&escape)).is_err(),
                "{escape:?}"
            );
        }
    }

    #[test]
    fn ordinary_flags_and_values_still_pass() {
        let ws = std::env::temp_dir();
        for ok in [
            vec!["cargo", "test", "--", "--nocapture"],
            vec!["git", "log", "--format=%H", "-n", "5"],
            vec!["ls", "-la"],
            vec!["grep", "-rn", "needle", "."],
        ] {
            assert!(confine_exec_argv(&ws, &argv(&ok)).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn secure_path_rejects_escapes_and_symlinks() {
        let ws = tempfile::tempdir().unwrap();
        for bad in ["/etc/passwd", "../out", "a/../../x", ""] {
            assert!(super::secure_path(ws.path(), bad).is_err(), "{bad:?}");
        }
        // Existing file inside the workspace resolves canonically.
        std::fs::write(ws.path().join("ok.txt"), "x").unwrap();
        let resolved = super::secure_path(ws.path(), "ok.txt").unwrap();
        assert!(resolved.starts_with(ws.path().canonicalize().unwrap()));
        // A symlinked leaf is refused outright.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", ws.path().join("link")).unwrap();
            assert!(super::secure_path(ws.path(), "link").is_err());
            // A symlinked *parent* cannot smuggle an escape either.
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), ws.path().join("dirlink")).unwrap();
            assert!(super::secure_path(ws.path(), "dirlink/ok.txt").is_err());
        }
    }

    #[test]
    fn read_file_nofollow_reads_and_refuses_symlinks() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("f.txt"), "contents").unwrap();
        assert_eq!(
            super::read_file_nofollow(&ws.path().join("f.txt")).unwrap(),
            "contents"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", ws.path().join("l")).unwrap();
            assert!(super::read_file_nofollow(&ws.path().join("l")).is_err());
        }
    }

    #[test]
    fn tool_string_arg_accepts_string_or_named_keys() {
        assert_eq!(
            super::tool_string_arg(&serde_json::json!(" bare "), &["path"]).unwrap(),
            "bare"
        );
        assert_eq!(
            super::tool_string_arg(&serde_json::json!({"cmd": "ls"}), &["command", "cmd"]).unwrap(),
            "ls"
        );
        assert!(super::tool_string_arg(&serde_json::json!({}), &["path"]).is_err());
        assert!(super::tool_string_arg(&serde_json::json!({"path": " "}), &["path"]).is_err());
    }

    #[test]
    fn ssrf_blocklist_covers_internal_ranges() {
        use std::net::IpAddr;
        for blocked in [
            "127.0.0.1",
            "10.0.0.9",
            "192.168.1.1",
            "172.16.5.5",
            "169.254.1.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "fc00::1",
            "fe80::1",
            "::",
            "ff02::1",
            "::ffff:127.0.0.1",
        ] {
            let ip: IpAddr = blocked.parse().unwrap();
            assert!(super::is_blocked_ssrf_target(ip), "{blocked}");
        }
        for open in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            let ip: IpAddr = open.parse().unwrap();
            assert!(!super::is_blocked_ssrf_target(ip), "{open}");
        }
    }

    #[test]
    fn secure_external_url_blocks_non_http_and_internal_hosts() {
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com",
            "http://127.0.0.1:8080/",
            "http://10.0.0.1/",
            "http://[::1]/",
            "not a url",
            "http://",
        ] {
            assert!(super::secure_external_url(bad).is_err(), "{bad}");
        }
        // Literal public IP resolves without DNS.
        assert!(super::secure_external_url("http://8.8.8.8/").is_ok());
    }

    #[test]
    fn proc_readers_return_real_tables() {
        let ps = super::os_ps_proc(5, "").unwrap();
        assert!(ps.starts_with("PID\tNAME\tRSS_KB\n"));
        assert!(ps.lines().count() > 1);
        let filtered = super::os_ps_proc(5, "definitely-no-such-comm").unwrap();
        assert_eq!(filtered.lines().count(), 1);
        let info = super::os_sysinfo_proc().unwrap();
        assert!(info.contains("kernel:"));
        assert!(info.contains("cpus:"));
    }

    #[test]
    fn external_agent_control_requires_task_id() {
        assert!(super::external_agent_control(
            &serde_json::json!({}),
            std::path::Path::new("."),
            "status"
        )
        .is_err());
    }
}
