use super::*;
#[test]
fn mcp_launcher_native_lookup_does_not_execute_or_accept_relative_path() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    fs::write(dir.path().join("npx"), "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path().join("npx"), fs::Permissions::from_mode(0o700)).unwrap();
    }
    for name in ["npx.cmd", "uvx.exe"] {
        let path = dir.path().join(name);
        fs::write(&path, "not an executable program").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let stem = name.split('.').next().unwrap();
        assert_eq!(
            resolve_runner_in(stem, std::iter::once(dir.path().to_path_buf()), true),
            Some(path)
        );
    }
    assert!(resolve_runner_in("npx", std::iter::once(PathBuf::from(".")), true).is_none());
    assert!(
        resolve_runner_in("missing", std::iter::once(dir.path().to_path_buf()), true).is_none()
    );
}
#[test]
fn damaged_user_disabled_list_is_an_error_not_empty() {
    let dir = std::env::temp_dir().join(format!(
        "susi_user_disabled_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    assert!(load_user_disabled(&dir).unwrap().is_empty());
    mark_user_disabled(&dir, "github").unwrap();
    mark_user_disabled(&dir, "sentry").unwrap();
    assert_eq!(load_user_disabled(&dir).unwrap(), ["github", "sentry"]);
    fs::write(user_disabled_path(&dir), "[\"github\"").unwrap();
    assert!(load_user_disabled(&dir).is_err());
    assert!(mark_user_disabled(&dir, "context7").is_err());
    assert!(clear_user_disabled(&dir, "github").is_err());
    assert_eq!(
        fs::read_to_string(user_disabled_path(&dir)).unwrap(),
        "[\"github\""
    );
    let _ = fs::remove_dir_all(&dir);
}
/// A synthetic pack-format catalog row (the shape commercial packs ship
/// in `leading-mcp.json`); core bundles no real one.
fn synthetic_definition() -> LeadingMcpDefinition {
    serde_json::from_value(serde_json::json!({ "id": "acme-files", "name": "Acme Files MCP", "rank": 1, "documentation": "https://example.invalid/acme-files", "package": "@example/acme-files-mcp", "runner": "npx", "args": ["-y", "@example/acme-files-mcp", "{workspace}"], "env_keys": ["ACME_TOKEN"], "category": "fs"}))
    .expect("synthetic leading-mcp row")
}
#[test]
fn pack_catalog_rows_parse_and_validate() {
    let def = synthetic_definition();
    def.validate().unwrap();
    assert_eq!(def.id, "acme-files");
    let mut bad = def.clone();
    bad.runner = String::new();
    assert!(bad.validate().is_err());
    let mut bad = def;
    bad.env_keys = vec!["NOT-A-NAME".into()];
    assert!(bad.validate().is_err());
}
#[test]
fn expand_workspace_and_env_placeholders() {
    let _env = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("SUSI_TEST_PG", "postgresql://localhost/db");
    let args = expand_args(
        &[
            "{workspace}".into(),
            "{env:SUSI_TEST_PG}".into(),
            "literal".into(),
        ],
        Path::new("/tmp"),
    )
    .unwrap();
    assert!(args[0].contains("tmp") || args[0] == "/tmp");
    assert_eq!(args[1], "postgresql://localhost/db");
    assert_eq!(args[2], "literal");
    std::env::remove_var("SUSI_TEST_PG");
}
#[test]
fn expand_workspace_embedded_in_path_args() {
    let args = expand_args(
        &["--db-path".into(), "{workspace}/.susi/agent.sqlite".into()],
        Path::new("/tmp/ws"),
    )
    .unwrap();
    assert_eq!(args[1], "/tmp/ws/.susi/agent.sqlite");
    let _env = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("SUSI_TEST_EMBED", "sekrit");
    let args = expand_args(&["x-{env:SUSI_TEST_EMBED}".into()], Path::new("/tmp")).unwrap();
    assert_eq!(args[0], "x-{env:SUSI_TEST_EMBED}");
    std::env::remove_var("SUSI_TEST_EMBED");
}
#[test]
fn override_store_round_trip() {
    let root = std::env::temp_dir().join(format!(
        "susi-leading-mcp-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let overrides = root.join("overrides");
    let def = synthetic_definition();
    let over = LeadingMcpOverride {
        runner: Some("uvx".into()),
        args: Some(vec!["acme-files-mcp".into()]),
        env_keys: Some(vec![]),
        package: None,
        ..Default::default()
    };
    crate::susi_config::write_json_override(&overrides, &def.id, &over).unwrap();
    let stored: LeadingMcpOverride =
        serde_json::from_slice(&fs::read(overrides.join("acme-files.json")).unwrap()).unwrap();
    assert_eq!(stored.runner.as_deref(), Some("uvx"));
    crate::susi_config::clear_json_override(&overrides, &def.id).unwrap();
    assert!(!overrides.join("acme-files.json").exists());
    let _ = fs::remove_dir_all(root);
}
/// The explicit admission path from an installed pack: with a synthetic
/// pack's one-row `leading-mcp.json` loaded, nothing is enabled on its
/// own; `configure` / `reset` touch only the per-id override; `enable`
/// writes exactly that server and `disable` removes it. A configured
/// server with no catalog entry is listed and removable too.
#[test]
fn manager_admits_a_pack_row_only_on_explicit_request() {
    let Some(dir) = crate::private_config_fixture() else {
        return;
    };
    let bin = dir.join("acme-runner");
    fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let pack = dir.join("extensions").join("acme");
    fs::create_dir_all(&pack).unwrap();
    fs::write(
        pack.join("manifest.json"),
        serde_json::json!({ "id": "acme", "name": "Acme", "version": "0.0.1", "files": { "leading-mcp.json": "leading-mcp.json"}})
        .to_string(),
    )
    .unwrap();
    let mut row = serde_json::to_value(synthetic_definition()).unwrap();
    row["runner"] = serde_json::json!(bin.display().to_string());
    row["env_keys"] = serde_json::json!([]);
    fs::write(
        pack.join("leading-mcp.json"),
        serde_json::json!([row]).to_string(),
    )
    .unwrap();
    crate::susi_sandbox::extensions::load_pack("acme").unwrap();
    let ws = dir.join("ws");
    fs::create_dir_all(&ws).unwrap();
    let ws = ws.canonicalize().unwrap();
    let overrides = dir.join("leading-mcp");
    let manager = LeadingMcpManager::with_config(ws.clone(), overrides.clone());
    let mcp_config = GmcpClient::get_config_path();
    assert!(!manager.is_enabled("acme-files").unwrap());
    assert!(!mcp_config.exists(), "nothing is admitted on its own");
    let over = LeadingMcpOverride {
        args: Some(vec!["--stdio".into()]),
        ..Default::default()
    };
    assert_eq!(
        manager.configure("acme-files", &over).unwrap().args,
        ["--stdio"]
    );
    assert!(overrides.join("acme-files.json").is_file());
    assert_eq!(manager.effective("acme-files").unwrap().args, ["--stdio"]);
    assert!(!mcp_config.exists(), "configure enables nothing");
    let reset = manager.reset("acme-files").unwrap();
    assert_eq!(reset.args, ["-y", "@example/acme-files-mcp", "{workspace}"]);
    assert!(!overrides.join("acme-files.json").exists());
    let srv = manager.enable("acme-files").unwrap();
    assert_eq!(srv.command, bin.display().to_string());
    assert_eq!(srv.args.last().unwrap(), &ws.display().to_string());
    let cfg = GmcpClient::read_mcp_config(&mcp_config).unwrap();
    assert_eq!(cfg.mcp_servers.len(), 1);
    assert!(cfg.mcp_servers.contains_key("acme-files"));
    assert!(manager.disable("acme-files").unwrap());
    assert!(GmcpClient::read_mcp_config(&mcp_config)
        .unwrap()
        .mcp_servers
        .is_empty());
    assert!(load_user_disabled(&overrides)
        .unwrap()
        .contains(&"acme-files".to_string()));
    let admitted = GmcpClient::admit_mcp_server("legacy", &bin.display().to_string(), &[]);
    assert_eq!(admitted, "SUCCESS_ADMITTED");
    let rows = manager.status().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["mcp"]["id"] == "legacy" && r["catalog"] == false && r["enabled"] == true),
        "{rows:?}"
    );
    assert!(manager.disable("legacy").unwrap());
    assert!(manager.disable("not-configured").is_err());
}
#[test]
fn enabled_config_launcher_overrides_catalog_runner() {
    let root = std::env::temp_dir().join(format!(
        "susi-mcp-preflight-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let bin = root.join("npx");
    fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
    }
    let def = LeadingMcpDefinition {
        id: "github".into(),
        name: "GitHub MCP".into(),
        rank: 2,
        documentation: String::new(),
        package: "ghcr.io/github/github-mcp-server".into(),
        runner: "docker".into(),
        args: vec!["run".into()],
        env_keys: vec!["SUSI_TEST_MCP_ONLY".into()],
        category: "vcs".into(),
        notes: String::new(),
    };
    let cfg = McpServerConfig {
        command: bin.display().to_string(),
        args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
        env: Some(HashMap::from([(
            "SUSI_TEST_MCP_ONLY".into(),
            "present".into(),
        )])),
        extra: HashMap::new(),
    };
    let msg = preflight_definition(&def, Some(&cfg), &root).unwrap();
    assert!(msg.contains("enabled config"), "{msg}");
    let err = preflight_definition(&def, None, &root)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("docker") || err.contains("SUSI_TEST_MCP_ONLY"),
        "{err}"
    );
    let _ = fs::remove_dir_all(root);
}
