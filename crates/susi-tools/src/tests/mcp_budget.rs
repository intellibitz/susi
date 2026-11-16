use super::*;
use susi_core::mac_policy::PrivacyMode;

fn server(command: &str, capabilities: Option<Value>) -> McpServerConfig {
    let mut extra = HashMap::new();
    if let Some(c) = capabilities {
        extra.insert("capabilities".to_string(), c);
    }
    McpServerConfig {
        command: command.to_string(),
        args: Vec::new(),
        env: None,
        extra,
    }
}

fn local_only() -> MacPolicy {
    MacPolicy::new([7u8; 32], PrivacyMode::LocalOnly, false)
}

#[test]
fn subprocess_server_requires_process_exec() {
    let err = authorize_transport(&server("npx", None), &local_only()).unwrap_err();
    assert!(err.contains(actions::PROCESS_EXEC), "{err}");
    assert!(err.contains("`npx`"), "{err}");

    let balanced = MacPolicy::new([7u8; 32], PrivacyMode::Balanced, false);
    assert_eq!(authorize_transport(&server("npx", None), &balanced), Ok(()));

    let policy = local_only();
    policy
        .grant("susi", actions::PROCESS_EXEC, "npx", None)
        .unwrap();
    assert_eq!(authorize_transport(&server("npx", None), &policy), Ok(()));
}

#[test]
fn remote_http_requires_network_egress_but_loopback_does_not() {
    let policy = local_only();
    let err =
        authorize_transport(&server("https://mcp.example.com/mcp", None), &policy).unwrap_err();
    assert!(err.contains(actions::NETWORK_EGRESS), "{err}");

    for loopback in [
        "http://127.0.0.1:9090/mcp",
        "http://localhost:9093",
        "http://[::1]:9090/mcp",
    ] {
        assert_eq!(
            authorize_transport(&server(loopback, None), &policy),
            Ok(()),
            "{loopback} must stay local"
        );
    }

    let open = MacPolicy::new([7u8; 32], PrivacyMode::Open, false);
    assert_eq!(
        authorize_transport(&server("https://mcp.example.com/mcp", None), &open),
        Ok(())
    );
}

#[test]
fn declared_capabilities_are_validated_and_gated() {
    let policy = local_only();
    let local = "http://127.0.0.1:9090/mcp";

    assert_eq!(
        authorize_transport(
            &server(local, Some(json!(["tools", "filesystem"]))),
            &policy
        ),
        Ok(())
    );

    let err = authorize_transport(&server(local, Some(json!(["network"]))), &policy).unwrap_err();
    assert!(err.contains(actions::NETWORK_EGRESS), "{err}");

    let err = authorize_transport(&server(local, Some(json!(["tools", "telepathy"]))), &policy)
        .unwrap_err();
    assert_eq!(err, "MCP capability `telepathy` is not supported");

    let err = authorize_transport(&server(local, Some(json!([42]))), &policy).unwrap_err();
    assert_eq!(err, "MCP capability must be a string");

    let err = authorize_transport(&server(local, Some(json!("tools"))), &policy).unwrap_err();
    assert_eq!(err, "MCP capabilities must be a list");

    assert_eq!(
        authorize_transport(&server(local, Some(json!([]))), &policy),
        Ok(())
    );
}

#[test]
fn transport_denial_precedes_capability_validation() {
    let err =
        authorize_transport(&server("npx", Some(json!("not-a-list"))), &local_only()).unwrap_err();
    assert!(err.contains(actions::PROCESS_EXEC), "{err}");
}

#[test]
fn balanced_policy_permits_loopback_and_subprocess_with_filesystem_capability() {
    let balanced = MacPolicy::new([9u8; 32], PrivacyMode::Balanced, false);
    assert_eq!(
        authorize_transport(
            &server("npx", Some(json!(["tools", "filesystem"]))),
            &balanced
        ),
        Ok(())
    );
}

#[test]
fn registered_http_scope_uses_parsed_authority_and_omits_credentials() {
    let mut config = http_config(HashMap::new());
    for (url, resource, local) in [
        (
            "HTTP://user:password@EXAMPLE.org:80?token=secret#ignore",
            "http://example.org/mcp",
            false,
        ),
        (
            "http://localhost.evil.test/private",
            "http://localhost.evil.test/private",
            false,
        ),
        (
            "http://127.0.0.1@bad.invalid/",
            "http://bad.invalid/mcp",
            false,
        ),
        ("http://192.168.1.4/", "http://192.168.1.4/mcp", false),
        ("http://localhost:4321/", "http://localhost:4321/mcp", true),
        ("http://[::1]:4321/", "http://[::1]:4321/mcp", true),
        ("http://127.0.0.2/", "http://127.0.0.2/mcp", true),
    ] {
        config.command = url.to_string();
        assert_eq!(
            config.http_scope().unwrap(),
            Some((resource.to_string(), local))
        );
    }
}

fn http_config(extra: HashMap<String, Value>) -> McpServerConfig {
    McpServerConfig {
        command: "http://127.0.0.1:1".to_string(),
        args: vec![],
        env: None,
        extra,
    }
}

#[test]
fn connect_failure_cooldown_short_circuits_respawn() {
    let config = McpServerConfig {
        command: "/nonexistent-susi-test-binary-xyz".to_string(),
        args: vec![],
        env: None,
        extra: HashMap::new(),
    };
    let first = list_tools_blocking(
        config.clone(),
        Duration::from_secs(2),
        Duration::from_secs(2),
    );
    assert!(first.is_err());
    let key = serde_json::to_value(&config).expect("json").to_string();
    assert!(CONNECT_FAILURES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .contains_key(&key));
    let start = Instant::now();
    let second = list_tools_blocking(config, Duration::from_secs(2), Duration::from_secs(2));
    assert!(second
        .unwrap_err()
        .contains("connect cooled after a recent failure"));
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn http_transport_builds_config_and_fails_closed() {
    let mut extra = HashMap::new();
    extra.insert("auth_token".to_string(), Value::String("tok".to_string()));
    extra.insert("headers".to_string(), json!({"x-susi": "1"}));
    let err = list_tools_blocking(
        http_config(extra),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect_err("port 1 must refuse");
    assert!(!err.is_empty());
}

#[test]
fn http_transport_rejects_non_string_header_values() {
    let mut extra = HashMap::new();
    extra.insert("headers".to_string(), json!({"x-susi": 1}));
    let err = list_tools_blocking(
        http_config(extra),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect_err("numeric header must be rejected");
    assert!(err.contains("header values must be strings"));
}

#[test]
fn http_transport_rejects_invalid_header_names() {
    let mut extra = HashMap::new();
    extra.insert("headers".to_string(), json!({"bad header name": "v"}));
    assert!(list_tools_blocking(
        http_config(extra),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .is_err());
}

#[test]
fn http_transport_rejects_unparseable_command_url() {
    let mut config = http_config(HashMap::new());
    config.command = "http://".to_string();
    assert!(list_tools_blocking(config, Duration::from_secs(1), Duration::from_secs(1)).is_err());
}

#[test]
fn stdio_transport_honours_cwd_workspace_template() {
    let mut extra = HashMap::new();
    extra.insert("cwd".to_string(), Value::String("{workspace}".to_string()));
    let config = McpServerConfig {
        command: "/nonexistent-susi-test-binary-abc".to_string(),
        args: vec![],
        env: None,
        extra,
    };
    assert!(list_tools_blocking(config, Duration::from_secs(1), Duration::from_secs(1)).is_err());
}

#[test]
fn call_blocking_result_returns_errors_not_panics() {
    let config = McpServerConfig {
        command: "/nonexistent-susi-test-binary-xyz".to_string(),
        args: vec![],
        env: None,
        extra: HashMap::new(),
    };
    let res = call_blocking_result(
        config,
        "any_tool".to_string(),
        json!({}),
        Duration::from_secs(2),
        Duration::from_secs(2),
    );
    assert!(res.is_err());
}

/// The stdio sink starts its child with `CREATE_NO_WINDOW`, observed in
/// the child: it is alone in a console of its own (a child without the
/// flag shares the runner's console) and no window shows that console (a
/// child of a console-less parent, such as the detached daemon, would
/// get a visible one). Args, `config.env` and the cleared environment
/// are kept. The probe is PowerShell reading Win32, not an MCP server.
#[cfg(windows)]
#[test]
fn stdio_child_runs_in_its_own_hidden_console() {
    let powershell = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()),
    )
    .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let script = "Add-Type -Name W -Namespace N -MemberDefinition \
            '[DllImport(\"kernel32.dll\")] public static extern System.IntPtr GetConsoleWindow(); \
             [DllImport(\"user32.dll\")] public static extern bool IsWindowVisible(System.IntPtr h); \
             [DllImport(\"kernel32.dll\")] public static extern uint GetConsoleProcessList(uint[] l, uint n);'; \
            $h = [N.W]::GetConsoleWindow(); $l = New-Object 'System.UInt32[]' 64; \
            Write-Output ('attached=' + [N.W]::GetConsoleProcessList($l, 64) + \
            ' visible=' + ($h -ne [System.IntPtr]::Zero -and [N.W]::IsWindowVisible($h)) + \
            ' marker=' + $env:SUSI_NOWINDOW_MARKER + ' cargo=' + [bool]$env:CARGO_MANIFEST_DIR)";
    let config = McpServerConfig {
        command: powershell.to_string_lossy().into_owned(),
        args: ["-NoProfile", "-NonInteractive", "-Command", script]
            .map(String::from)
            .to_vec(),
        env: Some(HashMap::from([(
            "SUSI_NOWINDOW_MARKER".to_string(),
            "kept".to_string(),
        )])),
        extra: HashMap::new(),
    };
    let mut child = spawn_stdio_command(&config).unwrap();
    drop(child.stdin.take());
    // Bounded: a test fails, never hangs.
    let deadline = Instant::now() + Duration::from_secs(120);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("powershell probe did not finish within 120 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "attached=1 visible=False marker=kept cargo=False",
        "{out:?}"
    );
}
