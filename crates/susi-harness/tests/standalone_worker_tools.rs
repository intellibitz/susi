//! Exercise the public worker registry in its own process and isolated native profile.
use std::fs;
use susi_harness::tools::{Policy, Registry};

fn http_catalog(name: &'static str) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        for method in ["initialize", "notifications/initialized", "tools/list"] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["method"], method);
            let body = match method {
                "initialize" => {
                    serde_json::json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}}})
                }
                "tools/list" => {
                    serde_json::json!({"tools":[{"name":name,"inputSchema":{"type":"object"}}]})
                }
                _ => {
                    stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    continue;
                }
            };
            let response =
                serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":body}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        }
    });
    (url, handle)
}

#[test]
fn standalone_directory_listing_preserves_native_results_and_policy() {
    let case = susi_paths::tempfile::tempdir().unwrap();
    let home = case.path().join("home");
    let workspace = case.path().join("native Ω");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&workspace).unwrap();
    std::env::set_var("SUSI_HOME", &home);
    std::env::set_var("SUSI_NO_REGISTRY_ENV", "1");
    let mut registry = Registry::builtin();
    let tool = "list_dir";
    let list = |args: &str| registry.execute(&workspace, tool, args, &[]);
    assert_eq!(list("{}"), "(empty directory)");
    fs::create_dir(workspace.join("café")).unwrap();
    fs::write(workspace.join("z"), "data").unwrap();
    assert_eq!(list("{}"), "café/\nz");
    assert_eq!(list(r#"{"path":" café "}"#), "(empty directory)");
    assert_eq!(
        list(r#"{"path":"none"}"#),
        "directory 'none' does not exist"
    );
    assert_eq!(
        list(r#"{"path":"../"}"#),
        "path traversal ('..') is not permitted"
    );
    assert!(list(r#"{"path":"z"}"#).starts_with("failed to read directory: "));
    let config = home.join("mcp_config.json");
    for (tool_name, expected) in [
        ("unrelated_read", "café/\nz"),
        ("worker_list_dir", "(1 unavailable)"),
    ] {
        let (url, server) = http_catalog(tool_name);
        let fixture = serde_json::json!({"mcp_servers":{"catalog":{"command":url,"args":[]}}});
        fs::write(&config, serde_json::to_vec(&fixture).unwrap()).unwrap();
        assert!(list("{}").contains(expected));
        server.join().unwrap();
    }
    fs::write(&config, "{").unwrap();
    assert!(list("{}").starts_with("tool 'list_dir' provider failed:"));
    let unsupported = serde_json::json!({"mcp_servers":{"offline":{"command":case.path().join("missing"), "args":[]}}});
    fs::write(&config, serde_json::to_vec(&unsupported).unwrap()).unwrap();
    assert!(list("{}").contains("(1 unavailable)"));
    let mut declaration = registry.get(tool).unwrap().clone();
    declaration.policy = Policy::Deny;
    registry.register(declaration);
    assert_eq!(
        registry.execute(&workspace, tool, "{", &[tool.into()]),
        "tool 'list_dir' is denied by policy"
    );
}
