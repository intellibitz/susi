//! PSP-03 / plan B2 invariant: every `tools/list` entry declares a typed
//! input schema (a non-empty `properties` object) or explicitly declares
//! that it takes no arguments. Only dynamically discovered proxies may use
//! the passthrough declaration: their backing server's schema is dropped at
//! discovery, a recorded deviation from the plan's B2 invariant.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use susi_gmcp::protocol::GmcpService;
use susi_gmcp::rmcp;
use susi_gmcp::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use susi_gmcp::tools::args::{core_input_schema, core_tool_definitions, PASSTHROUGH_KEY};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, PartialEq)]
enum Shape {
    Typed,
    NoArgs,
    Passthrough,
    Untyped,
}

fn shape(schema: &Value) -> Shape {
    if schema["type"] != "object" {
        return Shape::Untyped;
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    if properties.is_some_and(|p| !p.is_empty()) {
        return Shape::Typed;
    }
    if properties.is_some() && schema["additionalProperties"] == json!(false) {
        return Shape::NoArgs;
    }
    if schema[PASSTHROUGH_KEY] == json!(true) {
        return Shape::Passthrough;
    }
    Shape::Untyped
}

#[test]
fn every_core_tool_declares_typed_or_no_argument_schema() {
    let definitions = core_tool_definitions();
    assert!(definitions.len() >= 80, "{} core tools", definitions.len());
    let mut names = std::collections::BTreeSet::new();
    for tool in &definitions {
        assert!(
            names.insert(tool.name.to_string()),
            "duplicate {}",
            tool.name
        );
        let schema = Value::Object((*tool.input_schema).clone());
        let kind = shape(&schema);
        assert!(
            matches!(kind, Shape::Typed | Shape::NoArgs),
            "{} has {kind:?} schema {schema}",
            tool.name
        );
        // Every `required` key is a declared property.
        let properties = schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for key in schema["required"].as_array().cloned().unwrap_or_default() {
            let key = key.as_str().unwrap();
            assert!(
                properties.contains_key(key),
                "{}: required {key}",
                tool.name
            );
        }
        // The schema compiles under the same validator registered tools use.
        susi_gmcp::schema_check::Validator::compile(&schema)
            .unwrap_or_else(|e| panic!("{} schema does not compile: {e}", tool.name));
        assert_eq!(
            core_input_schema(&tool.name).as_deref(),
            Some(&*tool.input_schema)
        );
    }
}

#[test]
fn typed_schemas_match_handler_arguments() {
    let schema = |name: &str| Value::Object((*core_input_schema(name).unwrap()).clone());
    let write = schema("write_file");
    assert_eq!(write["required"], json!(["path", "content"]));
    let kill = schema("os_kill");
    assert_eq!(kill["required"], json!(["pid"]));
    assert_eq!(kill["properties"]["pid"]["type"], "integer");
    let patch = schema("apply_patch_cycle");
    assert_eq!(patch["required"], json!(["files"]));
    assert_eq!(shape(&schema("status")), Shape::NoArgs);
    assert!(core_input_schema("github:create_issue").is_none());
    // AgentManager::send refuses an empty message.
    let send = schema("agents_send");
    assert_eq!(send["required"], json!(["task_id", "message"]));
    // Fields the handler deserializes into `#[serde(default)]` non-Option
    // types are optional but never nullable (serde rejects null for them).
    let record = schema("commit_record");
    let fields = record["$defs"]["CommitRecord"]["properties"]
        .as_object()
        .cloned()
        .or_else(|| record["properties"].as_object().cloned())
        .unwrap();
    for key in ["seq", "leader", "term", "prev_epoch", "kind", "member_sig"] {
        assert!(
            !fields[key]["type"].to_string().contains("null"),
            "commit_record.{key} admits null: {}",
            fields[key]
        );
    }
    for key in ["auto_apply", "description"] {
        assert!(
            !patch["properties"][key]["type"]
                .to_string()
                .contains("null"),
            "apply_patch_cycle.{key} admits null"
        );
    }
    // The formerly bare-string handlers read an object over MCP (plan B4
    // group 3): `select_model {model}` and `scout_model {url}`.
    assert_eq!(schema("select_model")["required"], json!(["model"]));
    assert_eq!(schema("scout_model")["required"], json!(["url"]));
    // A provider override is spelled `override` on the wire.
    let configure = schema("provider_configure");
    assert!(
        configure["properties"]["override"].is_object(),
        "{configure}"
    );
    assert_eq!(configure["required"], json!(["vendor", "model"]));
    // B4 group 11 (PSP-20): extension pack admission tools.
    assert_eq!(shape(&schema("extensions_list")), Shape::NoArgs);
    assert_eq!(shape(&schema("extensions_status")), Shape::NoArgs);
    let ext_validate = schema("extensions_validate");
    assert_eq!(ext_validate["required"], json!(["path"]));
    assert_eq!(ext_validate["properties"]["path"]["type"], "string");
    for name in ["extensions_load", "extensions_unload"] {
        let s = schema(name);
        assert_eq!(s["required"], json!(["id"]));
        assert_eq!(s["properties"]["id"]["type"], "string");
    }
}

/// Every tool the daemon bootstrap registers is served over MCP with its
/// handler's typed `inputSchema` (or an explicit no-argument schema), never
/// the passthrough shape reserved for dynamic proxies. Read from source so
/// the check spawns none of the registry's background work.
#[test]
fn every_registered_tool_has_a_typed_input_schema() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../susi-daemon/src/gmcp_bootstrap.rs");
    let source = std::fs::read_to_string(&path).unwrap();
    let body = source.split("#[cfg(test)]").next().unwrap_or(&source);
    let mut registered = Vec::new();
    let calls: Vec<&str> = body.split("ToolRegistry::register_meta_tool(").collect();
    for (before, call) in calls.iter().zip(calls.iter().skip(1)) {
        // A registration behind `tools-rich` exists only when that feature is
        // on; its typed schema is gated the same way in tools/args.rs.
        if before
            .trim_end()
            .ends_with("#[cfg(feature = \"tools-rich\")]")
            && !cfg!(feature = "tools-rich")
        {
            continue;
        }
        let name = call.split('"').nth(1).unwrap();
        let handler = call
            .split("adapt(CoreTools::")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .unwrap();
        assert_eq!(
            name, handler,
            "tool {name} dispatches to CoreTools::{handler}"
        );
        registered.push(name.to_string());
    }
    assert!(registered.len() >= 80, "{} registrations", registered.len());
    for name in &registered {
        let schema =
            core_input_schema(name).unwrap_or_else(|| panic!("{name} has no typed inputSchema"));
        assert!(
            !schema.contains_key(PASSTHROUGH_KEY),
            "{name} is declared passthrough"
        );
    }
}

async fn call(wire: &mut BufReader<tokio::io::DuplexStream>, message: Value) -> Value {
    wire.get_mut()
        .write_all(
            format!(
                "{message}
"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), wire.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

struct ListedTools(Vec<String>);
impl PlaneHandler for ListedTools {
    fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
        Ok(Value::Array(
            self.0
                .iter()
                .map(|name| json!({"name": name, "description": "listed"}))
                .collect(),
        ))
    }
}

#[tokio::test]
async fn every_tools_list_entry_is_typed_over_the_wire() {
    const PROXY: &str = "github:create_issue";
    let mut listed: Vec<String> = core_tool_definitions()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    listed.push(PROXY.to_string());
    PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(ListedTools(listed.clone())));

    let (client, server) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let running = GmcpService::new(".".into()).serve(server).await.unwrap();
        running.waiting().await.unwrap();
    });
    let mut wire = BufReader::new(client);
    call(&mut wire, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"schema-test","version":"1"}}})).await;
    let mut tools = Vec::new();
    let mut params = json!({});
    for id in 2.. {
        let page = call(
            &mut wire,
            json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":params}),
        )
        .await;
        tools.extend(page["result"]["tools"].as_array().unwrap().clone());
        match page["result"]["nextCursor"].as_str() {
            Some(cursor) => params = json!({"cursor": cursor}),
            None => break,
        }
    }
    for name in &listed {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name.as_str())
            .unwrap_or_else(|| panic!("{name} missing from tools/list"));
        let kind = shape(&tool["inputSchema"]);
        if name == PROXY {
            assert_eq!(kind, Shape::Passthrough, "{tool}");
        } else {
            assert!(
                matches!(kind, Shape::Typed | Shape::NoArgs),
                "{name} lists {kind:?}: {tool}"
            );
        }
    }
    // No entry is untyped; passthrough is reserved for dynamic proxies.
    for tool in &tools {
        let kind = shape(&tool["inputSchema"]);
        assert!(
            kind == Shape::Typed
                || kind == Shape::NoArgs
                || (kind == Shape::Passthrough && tool["name"] == PROXY),
            "tools/list entry without a typed schema: {tool}"
        );
    }
}
