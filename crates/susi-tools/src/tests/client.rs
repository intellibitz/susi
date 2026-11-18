use super::*;
#[test]
fn package_runner_selects_only_the_matching_launcher() {
    assert_eq!(
        package_runner("pypi:server-test"),
        Some(("uvx", "server-test"))
    );
    assert_eq!(
        package_runner("npm:@example/python-tools"),
        Some(("npx", "@example/python-tools"))
    );
    assert_eq!(package_runner("ghcr.io/github/github-mcp-server"), None);
    assert_eq!(package_runner("npm:-rf"), None);
}
/// A registry read writes no `global_mcp_registry.json` (so it never
/// freezes the registry), and an extension pack installed and loaded
/// afterwards supplies the rows.
#[test]
fn scout_registry_reads_write_nothing_and_follow_the_loaded_pack() {
    let Some(dir) = crate::private_config_fixture() else {
        return;
    };
    let host_file = dir.join("global_mcp_registry.json");
    assert!(GmcpClient::fetch_global_registry().is_empty());
    assert!(!host_file.exists(), "a registry read must write nothing");
    let pack = dir.join("extensions").join("acme");
    fs::create_dir_all(&pack).unwrap();
    fs::write(
        pack.join("manifest.json"),
        r#"{"id":"acme","name":"Acme","version":"0.0.1",
            "files":{"mcp-registry.json":"mcp-registry.json"}}"#,
    )
    .unwrap();
    fs::write(
        pack.join("mcp-registry.json"),
        r#"[{"name":"acme-mcp","description":"Acme","package":"npm:@acme/mcp","category":"reasoning"}]"#,
    )
    .unwrap();
    crate::susi_sandbox::extensions::load_pack("acme").unwrap();
    let names: Vec<String> = GmcpClient::fetch_global_registry()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["acme-mcp"]);
    assert!(!host_file.exists(), "a registry read must write nothing");
}
#[test]
fn core_bundles_no_mcp_scout_registry() {
    assert!(crate::susi_sandbox::manager::SusiConfig::default()
        .bootstrap_mcp_servers::<Vec<GlobalMcpEntry>>()
        .is_empty());
}
#[test]
fn admit_mcp_server_writes_http_and_stdio() {
    let Some(dir) = crate::private_config_fixture() else {
        return;
    };
    let res = GmcpClient::admit_mcp_server("remote", "http://127.0.0.1:3100/mcp", &[]);
    assert_eq!(res, "SUCCESS_ADMITTED");
    let res2 = GmcpClient::admit_mcp_server(
        "fs",
        "npx",
        &[
            "-y".into(),
            "@modelcontextprotocol/server-filesystem".into(),
        ],
    );
    assert_eq!(res2, "SUCCESS_ADMITTED");
    assert_eq!(GmcpClient::get_config_path(), dir.join("mcp_config.json"));
    let tools = GmcpClient::list_external_tools();
    let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.iter().any(|n| n.starts_with("remote:")), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("fs:")), "{names:?}");
}
#[test]
fn admit_refuses_damaged_config_without_erasing_servers() {
    if crate::private_config_fixture().is_none() {
        return;
    }
    let path = GmcpClient::get_config_path();
    let original = "{\"mcp_servers\":{\"keep\":{\"command\":\"echo\",\"args\":[]}},\"torn\"";
    fs::write(&path, original).unwrap();
    let res = GmcpClient::admit_mcp_server("added", "npx", &["-y".into()]);
    assert_eq!(res, "ERROR_CONFIG_DAMAGED");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}
#[test]
fn admit_refuses_unreadable_config_without_replacing_it() {
    if crate::private_config_fixture().is_none() {
        return;
    }
    let path = GmcpClient::get_config_path();
    fs::create_dir(&path).unwrap();
    let res = GmcpClient::admit_mcp_server("added", "npx", &[]);
    assert_eq!(res, "ERROR_CONFIG_UNREADABLE");
    assert!(
        path.is_dir(),
        "unreadable config must not be replaced by a file"
    );
}
