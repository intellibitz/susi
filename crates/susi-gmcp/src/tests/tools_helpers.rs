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
    let ws = susi_paths::tempfile::tempdir().unwrap();
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
        let outside = susi_paths::tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.path().join("dirlink")).unwrap();
        assert!(super::secure_path(ws.path(), "dirlink/ok.txt").is_err());
    }
}

#[test]
fn read_file_nofollow_reads_and_refuses_symlinks() {
    let ws = susi_paths::tempfile::tempdir().unwrap();
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
    let blocked = "127.0.0.1|10.0.0.9|192.168.1.1|172.16.5.5|169.254.1.1|0.0.0.0|224.0.0.1|255.255.255.255|::1|fc00::1|fe80::1|::|ff02::1|::ffff:127.0.0.1";
    for addr in blocked.split('|') {
        assert!(
            super::is_blocked_ssrf_target(addr.parse::<IpAddr>().unwrap()),
            "{addr}"
        );
    }
    for open in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
        assert!(
            !super::is_blocked_ssrf_target(open.parse::<IpAddr>().unwrap()),
            "{open}"
        );
    }
}

#[test]
fn secure_external_url_blocks_non_http_and_internal_hosts() {
    for bad in "file:///etc/passwd|ftp://example.com|http://127.0.0.1:8080/|http://10.0.0.1/|http://[::1]/|not a url|http://".split('|') {
        assert!(super::secure_external_url(bad).is_err(), "{bad}");
    }
    // Literal public IP resolves without DNS.
    assert!(super::secure_external_url("http://8.8.8.8/").is_ok());
}

#[test]
fn proc_readers_return_real_tables() {
    let ps = super::os_ps_proc(5, "").unwrap();
    assert!(ps.starts_with("PID\tNAME\tRSS_KB\n") && ps.lines().count() > 1);
    assert_eq!(
        super::os_ps_proc(5, "definitely-no-such-comm")
            .unwrap()
            .lines()
            .count(),
        1
    );
    let info = super::os_sysinfo_proc().unwrap();
    assert!(info.contains("kernel:") && info.contains("cpus:"));
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
