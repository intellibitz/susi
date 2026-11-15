use super::*;
use susi_paths::tempfile::tempdir;

#[test]
fn test_format_tool_response_success_and_error() {
    let ok_val = format_tool_response("success payload", false);
    assert_eq!(ok_val["isError"], false);
    assert_eq!(ok_val["content"][0]["type"], "text");
    assert_eq!(ok_val["content"][0]["text"], "success payload");

    let err_val = format_tool_response("failed payload", true);
    assert_eq!(err_val["isError"], true);
    assert_eq!(err_val["content"][0]["type"], "text");
    assert_eq!(err_val["content"][0]["text"], "failed payload");
}

#[test]
fn test_validate_tool_args_non_object_rejected() {
    let err = validate_tool_args("susi", "status", &json!("not-an-object")).unwrap_err();
    assert!(err.to_string().contains("JSON object"));
}

#[test]
fn test_validate_tool_args_built_in_schemas() {
    // read_file requires "path"
    let ok_args = json!({ "path": "Cargo.toml" });
    assert!(validate_tool_args("susi", "read_file", &ok_args).is_ok());

    let bad_args = json!({ "wrong_key": 123 });
    let err = validate_tool_args("susi", "read_file", &bad_args).unwrap_err();
    assert!(err.to_string().contains("inputSchema"));

    // write_file requires "path" and "content"
    let bad_write = json!({ "path": "file.txt" });
    assert!(validate_tool_args("susi", "write_file", &bad_write).is_err());
    let ok_write = json!({ "path": "file.txt", "content": "hello" });
    assert!(validate_tool_args("susi", "write_file", &ok_write).is_ok());
}

#[test]
fn test_custom_schema_registration_and_enforcement() {
    register_tool_schema(
        "custom_srv",
        "calc",
        json!({
            "type": "object",
            "properties": {
                "num": { "type": "integer", "minimum": 10 }
            },
            "required": ["num"]
        }),
    );

    let invalid = json!({ "num": 5 });
    assert!(validate_tool_args("custom_srv", "calc", &invalid).is_err());

    let valid = json!({ "num": 42 });
    assert!(validate_tool_args("custom_srv", "calc", &valid).is_ok());
}

#[test]
fn test_call_tool_direct_schema_validation_rejection_formats_error_and_records_evidence() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path();

    // Create an explicit evidence session
    let sess =
        susi_core::capture::EvidenceSession::new("test-mission", ws, |s| s.to_string()).unwrap();
    let _act = susi_core::capture::EvidenceSession::activate(&sess);

    // Call read_file without required path
    let res = call_tool_direct("susi", "read_file", &json!({}), ws).unwrap();
    assert_eq!(res["isError"], true);
    assert!(res["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("inputSchema"));

    // Evidence ledger should contain the recorded call
    assert!(
        !sess.receipts().is_empty(),
        "Evidence session must record the invalid call"
    );
}

#[test]
fn test_call_tool_direct_native_execution_and_evidence() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path();

    let sess =
        susi_core::capture::EvidenceSession::new("test-exec", ws, |s| s.to_string()).unwrap();
    let _act = susi_core::capture::EvidenceSession::activate(&sess);

    let res = call_tool_direct("susi", "status", &json!({}), ws).unwrap();
    assert_eq!(res["isError"], false);
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("SUSI Engine Version"));

    assert!(!sess.receipts().is_empty());
}

#[test]
fn test_custom_handler_registration_and_execution() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path();

    register_tool_handler(
        "mock_server",
        "greet",
        std::sync::Arc::new(|args, _ws| {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("World");
            Ok(format!("Hello, {name}!"))
        }),
    );

    let res = call_tool_direct(
        "mock_server",
        "greet",
        &json!({ "name": "Antigravity" }),
        ws,
    )
    .unwrap();
    assert_eq!(res["isError"], false);
    assert_eq!(res["content"][0]["text"], "Hello, Antigravity!");
}

#[test]
fn test_read_file_write_file_round_trip_via_call_tool_direct() {
    let tmp = tempdir().unwrap();
    let ws = tmp.path();

    // 1. Write file
    let write_res = call_tool_direct(
        "susi",
        "write_file",
        &json!({ "path": "test.txt", "content": "hello mcp direct" }),
        ws,
    )
    .unwrap();
    assert_eq!(write_res["isError"], false);

    // 2. Read file back
    let read_res =
        call_tool_direct("susi", "read_file", &json!({ "path": "test.txt" }), ws).unwrap();
    assert_eq!(read_res["isError"], false);
    assert_eq!(read_res["content"][0]["text"], "hello mcp direct");
}
