//! Exercise the public worker registry in its own process and isolated native profile.
use std::fs;
use susi_harness::tools::{Policy, Registry};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn http_catalog(
    name: Option<&'static str>,
) -> TestResult<(String, std::thread::JoinHandle<TestResult>)> {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    let handle = std::thread::spawn(move || -> TestResult {
        for method in ["initialize", "notifications/initialized", "tools/list"] {
            let (mut stream, _) = listener.accept()?;
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line)? > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse()?;
                    }
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes)?;
            let request: serde_json::Value = serde_json::from_slice(&bytes)?;
            assert_eq!(request["method"], method);
            let body = match method {
                "initialize" => {
                    serde_json::json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}}})
                }
                "tools/list" => {
                    serde_json::json!({"tools":[{"name":name,"inputSchema":{"type":"object"}}]})
                }
                _ => {
                    stream.write_all(
                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )?;
                    continue;
                }
            };
            let response =
                serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":body}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response)?;
        }
        Ok(())
    });
    Ok((url, handle))
}

#[test]
fn standalone_directory_listing_preserves_native_results_and_policy() -> TestResult {
    let case = susi_paths::tempfile::tempdir()?;
    let home = case.path().join("home");
    let workspace = case.path().join("native Ω");
    fs::create_dir(&home)?;
    fs::create_dir(&workspace)?;
    std::env::set_var("SUSI_HOME", &home);
    std::env::set_var("SUSI_NO_REGISTRY_ENV", "1");
    let mut registry = Registry::builtin();
    let tool = "list_dir";
    let list = |args: &str| registry.execute(&workspace, tool, args, &[]);
    assert_eq!(list("{}"), "(empty directory)");
    fs::create_dir(workspace.join("café"))?;
    fs::write(workspace.join("z"), "data")?;
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
    let call = |tool, input: serde_json::Value| {
        registry.execute(&workspace, tool, &input.to_string(), &[])
    };
    let file = "nested/α.txt";
    let write = serde_json::json!({"path":file, "content":"café"});
    assert_eq!(
        call("write_to_file", write.clone()),
        format!("successfully wrote 5 bytes to '{file}'")
    );
    assert!(call("write_to_file", write).starts_with("refused:"));
    assert_eq!(
        call(
            "append_to_file",
            serde_json::json!({"path":file, "content":"第二\n"})
        ),
        format!("successfully appended 7 bytes to '{file}' (5 bytes before)")
    );
    assert_eq!(
        call("view_file", serde_json::json!({"path":file})),
        "    1: café\n    2: 第二\n"
    );
    assert_eq!(
        call(
            "view_file",
            serde_json::json!({"path":file,"start_line":2,"end_line":0})
        ),
        "invalid line range: end_line precedes start_line"
    );
    assert_eq!(
        call(
            "replace_file_content",
            serde_json::json!({"path":file, "target":"café","replacement":"one"})
        ),
        format!("successfully replaced target in '{file}'")
    );
    assert_eq!(fs::read_to_string(workspace.join(file))?, "one\n第二\n");
    assert_eq!(
        call("append_to_file", serde_json::json!({"path":"../x"})),
        "missing 'content' argument"
    );
    assert_eq!(
        call(
            "replace_file_content",
            serde_json::json!({"path":"x","target":"x"})
        ),
        "missing 'replacement' argument"
    );
    fs::remove_file(workspace.join(file))?;
    fs::remove_dir(workspace.join("nested"))?;
    let config = home.join("mcp_config.json");
    for (tool_name, expected) in [
        (Some("unrelated_read"), "café/\nz"),
        (Some("worker_list_dir"), "(1 unavailable)"),
        (Some(""), "(1 unavailable)"),
        (None, "(1 unavailable)"),
    ] {
        let (url, server) = http_catalog(tool_name)?;
        let fixture = serde_json::json!({"mcp_servers":{"catalog":{"command":url,"args":[]}}});
        fs::write(&config, serde_json::to_vec(&fixture)?)?;
        assert!(list("{}").contains(expected));
        server
            .join()
            .map_err(|_| std::io::Error::other("catalog fixture panicked"))??;
    }
    fs::write(&config, "{")?;
    assert!(list("{}").starts_with("tool 'list_dir' provider failed:"));
    let unsupported = serde_json::json!({"mcp_servers":{"offline":{"command":case.path().join("missing"), "args":[]}}});
    fs::write(&config, serde_json::to_vec(&unsupported)?)?;
    assert!(list("{}").contains("(1 unavailable)"));
    let mut declaration = registry
        .get(tool)
        .ok_or("list_dir must be registered")?
        .clone();
    declaration.policy = Policy::Deny;
    registry.register(declaration);
    assert_eq!(
        registry.execute(&workspace, tool, "{", &[tool.into()]),
        "tool 'list_dir' is denied by policy"
    );
    Ok(())
}
