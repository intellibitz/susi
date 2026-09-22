//! Path/SSRF/argv guards and external-agent task helpers for CoreTools.

use std::fs;
use std::path::{Component, Path, PathBuf};
use susi_error::{EaiError, EaiResult};

pub(super) fn manager_for_external_task(
    workspace: &Path,
    id: &str,
) -> EaiResult<susi_agents::external::AgentManager> {
    let execution = susi_agents::external::AgentManager::new(workspace)
        .map_err(|e| EaiError::process(e.to_string()))?;
    if execution.read(id).is_ok() {
        return Ok(execution);
    }
    let frameworks = susi_agents::external::AgentManager::frameworks(workspace)
        .map_err(|e| EaiError::process(e.to_string()))?;
    if frameworks.read(id).is_ok() {
        return Ok(frameworks);
    }
    Err(EaiError::protocol(format!("unknown task_id {id}")))
}

pub(super) fn external_agent_control(
    arg: &serde_json::Value,
    workspace: &Path,
    action: &str,
) -> EaiResult<String> {
    let id = arg
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| EaiError::protocol("task_id is required"))?;
    let manager = manager_for_external_task(workspace, id)?;
    if action == "logs" {
        return manager
            .logs(
                id,
                arg.get("stderr").and_then(|v| v.as_bool()).unwrap_or(false),
                65536,
            )
            .map_err(|e| EaiError::process(e.to_string()));
    }
    let run = match action {
        "cancel" => manager.cancel(id),
        "send" => manager.send(
            id,
            arg.get("message").and_then(|v| v.as_str()).unwrap_or(""),
        ),
        _ if arg
            .get("refresh")
            .and_then(|v| v.as_bool())
            .unwrap_or(false) =>
        {
            manager.refresh(id)
        }
        _ => manager.status(id),
    }
    .map_err(|e| EaiError::process(e.to_string()))?;
    serde_json::to_string(&run)
        .map(|s| susi_agents::external::redact(&s))
        .map_err(|e| EaiError::protocol(e.to_string()))
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

    // Canonicalize parent to block mid-path symlink escapes out of workspace.
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

    // Reject a symlink *leaf* (parent canonicalize alone lets `link -> /etc/passwd`
    // through). For existing paths, fully resolve and re-check the workspace root.
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

/// Open a workspace path for reading without following a raced-in symlink leaf
/// (`O_NOFOLLOW` on Unix). Pair with `secure_path` for TOCTOU-safe reads.
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

/// Reject absolute paths, `..`, and path-like argv that escape the workspace
/// so allowlisted bins (`cat`, `rg`, …) cannot read host FS outside cwd.
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

/// Blocks loopback, private, link-local (including the 169.254.169.254 cloud
/// metadata endpoint), unspecified, and multicast/broadcast targets, plus
/// their IPv4-mapped IPv6 form. This closes the direct SSRF vector (an
/// attacker-supplied URL pointing straight at internal infrastructure); it
/// does not close a DNS-rebinding variant, where a hostname resolves to a
/// public IP at check time and a private one when headless_chrome's own,
/// separate DNS lookup later connects.
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
            let is_unique_local = (segments[0] & 0xfe00) == 0xfc00; // fc00::/7
            let is_unicast_link_local = (segments[0] & 0xffc0) == 0xfe80; // fe80::/10
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_unique_local
                || is_unicast_link_local
        }
    }
}

/// Validates a user-supplied URL before it's handed to a browser/HTTP client:
/// only http(s) schemes, and every address the host resolves to must clear
/// [`is_blocked_ssrf_target`].
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

#[cfg(test)]
mod ssrf_guard_tests {
    use super::secure_external_url;

    #[test]
    fn test_blocks_loopback() {
        assert!(secure_external_url("http://127.0.0.1/admin").is_err());
        assert!(secure_external_url("http://127.0.0.1:8080/").is_err());
        assert!(secure_external_url("http://[::1]/").is_err());
    }

    #[test]
    fn test_blocks_private_ranges() {
        assert!(secure_external_url("http://10.0.0.1/").is_err());
        assert!(secure_external_url("http://172.16.0.1/").is_err());
        assert!(secure_external_url("http://192.168.1.1/").is_err());
    }

    #[test]
    fn test_blocks_cloud_metadata_endpoint() {
        // 169.254.169.254 is the AWS/GCP/Azure instance-metadata endpoint,
        // the single most common real-world SSRF exploitation target.
        assert!(secure_external_url("http://169.254.169.254/latest/meta-data/").is_err());
    }

    #[test]
    fn test_blocks_ipv4_mapped_ipv6_loopback() {
        assert!(secure_external_url("http://[::ffff:127.0.0.1]/").is_err());
    }

    #[test]
    fn test_blocks_non_http_schemes() {
        assert!(secure_external_url("file:///etc/passwd").is_err());
        assert!(secure_external_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn test_allows_public_ip_literal() {
        // IP-literal (no DNS) to keep this test hermetic.
        assert!(secure_external_url("http://93.184.216.34/").is_ok());
    }
}

#[cfg(test)]
mod secure_path_tests {
    use super::secure_path;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch_dir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("susi-secure-path-{}-{}", std::process::id(), nanos));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rejects_symlink_leaf_escape() {
        let dir = scratch_dir();
        let workspace = dir.join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let outside = dir.join("secret.txt");
        fs::write(&outside, "nope").unwrap();
        let link = workspace.join("escape");
        symlink(&outside, &link).unwrap();

        let err = secure_path(&workspace, "escape").unwrap_err();
        assert!(
            err.to_string().contains("Symlink"),
            "expected symlink rejection, got: {err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn allows_regular_file_inside_workspace() {
        let dir = scratch_dir();
        let workspace = dir.join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let file = workspace.join("ok.txt");
        fs::write(&file, "yes").unwrap();

        let got = secure_path(&workspace, "ok.txt").unwrap();
        assert_eq!(got, file.canonicalize().unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn allows_new_path_under_workspace() {
        let dir = scratch_dir();
        let workspace = dir.join("ws");
        fs::create_dir_all(&workspace).unwrap();

        let got = secure_path(&workspace, "new.txt").unwrap();
        assert_eq!(got, workspace.canonicalize().unwrap().join("new.txt"));
        let _ = fs::remove_dir_all(&dir);
    }
}
