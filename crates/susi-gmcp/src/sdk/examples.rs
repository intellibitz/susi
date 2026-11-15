//! Runnable minimal-through-production GMCP server authoring and plugin
//! lifecycle examples (`SDK-018`, `docs/gmcp/contract-203-020.md`).

use super::{
    mrtr_elicitation, mrtr_input_required, require_client_elicitation, PluginBundle, ServerBuilder,
};
use crate::extensions::ExtensionStatus;
use crate::mcp::{model::*, task_manager::TaskExit, ErrorData};
use crate::protocol::{GmcpService, ToolMiddleware};
use crate::sdk_schema;
use crate::susi_config::extensions::ExtensionManifest;
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};
use susi_paths::sync::RwLock;

sdk_schema! {
    /// Minimal echo input derived via Susi-owned `sdk_schema!`.
    pub struct EchoInput {
        /// Text to echo back to the caller.
        pub message: String,
        /// Optional uppercase conversion flag.
        #[serde(default)]
        pub uppercase: bool,
    }
    /// Nested filter descriptor for structured production tool calls.
    pub struct ScopeFilter {
        /// Required namespace prefix.
        pub namespace: String,
        /// Optional tag filter.
        pub tag: Option<String>,
    }
    /// Structured input for `summarize_items`.
    pub struct SummarizeInput {
        /// Items to summarize.
        pub items: Vec<String>,
        /// Nested scope filter.
        pub scope: ScopeFilter,
        /// Maximum items to include (`0` means all).
        #[serde(default, rename = "maxItems")]
        pub max_items: u32,
    }
    /// Structured output verified against compiled `outputSchema` before egress.
    pub struct SummarizeOutput {
        /// Count of processed items.
        pub count: u32,
        /// Joined summary text.
        pub summary: String,
        /// Echoed namespace from `scope`.
        pub namespace: String,
    }
    /// Input for the inline MRTR confirmation tool.
    pub struct ConfirmActionInput {
        /// Operation requiring operator confirmation.
        pub operation: String,
    }
    /// Input for the durable MRTR approval task workflow.
    pub struct ApprovalTaskInput {
        /// Change ticket identifier.
        pub ticket: String,
    }
    /// Input for the sample plugin's lookup tool.
    pub struct PluginLookupInput {
        /// Lookup key inside the plugin catalog.
        pub key: String,
    }
}

pub(crate) fn text_read_response(uri: String, mime: &str, text: String) -> ReadResourceResponse {
    ReadResourceResult {
        result_type: Some(ResultType::COMPLETE),
        ttl_ms: Some(0),
        cache_scope: Some(CacheScope::Private),
        contents: vec![ResourceContents::TextResourceContents {
            uri,
            mime_type: Some(mime.into()),
            text,
            meta: None,
        }],
        meta: None,
    }
    .into()
}

fn make_audit_middleware(tag: &'static str, log: AuditLog) -> ToolMiddleware {
    Arc::new(move |req, ctx, next| {
        let log = Arc::clone(&log);
        Box::pin(async move {
            log.write().push(format!("{tag}:before:{}", req.name));
            let res = next(req.clone(), ctx).await;
            log.write().push(format!("{tag}:after:{}", req.name));
            res
        })
    })
}

fn make_prompt(name: &str, title: &str, desc: &str, arg: Option<(&str, &str)>) -> Prompt {
    Prompt {
        name: name.into(),
        title: Some(title.into()),
        description: Some(desc.into()),
        arguments: arg.map(|(n, d)| {
            vec![PromptArgument {
                name: n.into(),
                title: Some(n.into()),
                description: Some(d.into()),
                required: Some(true),
            }]
        }),
        icons: None,
        meta: None,
    }
}

fn prompt_msg(role: Role, desc: String, text: String) -> GetPromptResponse {
    GetPromptResult {
        result_type: Some(ResultType::COMPLETE),
        description: Some(desc),
        messages: vec![PromptMessage {
            role,
            content: ContentBlock::text(text),
        }],
        meta: None,
    }
    .into()
}
pub fn minimal_echo_server(workspace: PathBuf) -> Result<GmcpService, ErrorData> {
    ServerBuilder::new("sdk-minimal-echo", "0.1.0", workspace)
        .title("SDK Minimal Echo Server")
        .description("Minimal single-tool local MCP server built with Susi ServerBuilder")
        .tool_typed(
            "echo",
            "Echo a caller message with optional uppercase transformation",
            |input: EchoInput, _raw, _ctx| async move {
                let text = if input.uppercase {
                    input.message.to_uppercase()
                } else {
                    input.message
                };
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
            },
        )
        .build()
}

/// Shared ordered middleware/handler audit log for the production reference server.
pub type AuditLog = Arc<RwLock<Vec<String>>>;

/// Full production reference server (`06_production_plugin_server`).
pub fn production_reference_server(
    workspace: PathBuf,
    task_store: PathBuf,
) -> Result<(GmcpService, AuditLog), ErrorData> {
    let audit_log: AuditLog = Arc::new(RwLock::new(Vec::new()));
    let mw_outer = make_audit_middleware("mw1", Arc::clone(&audit_log));
    let mw_inner = make_audit_middleware("mw2", Arc::clone(&audit_log));
    let log_handler = Arc::clone(&audit_log);

    let template = ResourceTemplate {
        uri_template: "memo://items/{id}".into(),
        name: "memo-item".into(),
        title: Some("Memo Item".into()),
        description: Some("Dynamic memo addressed by RFC 6570 item id".into()),
        mime_type: Some("application/json".into()),
        icons: None,
        meta: None,
        annotations: None,
    };
    let prompt = make_prompt(
        "draft_brief",
        "Draft Executive Brief",
        "Assemble a structured brief for a topic",
        Some(("topic", "Subject of the brief")),
    );
    let ext_settings = Map::from_iter([("profile".into(), json!("production-reference"))]);

    let service = ServerBuilder::new("sdk-production-ref", "1.2.0", workspace)
        .title("SDK Production Reference Server")
        .instructions("Reference local MCP server exercising every VC-203-020 SDK capability.")
        .durable_tasks(task_store)
        .middleware(mw_outer)
        .middleware(mw_inner)
        .tool_structured(
            "summarize_items",
            "Summarize scoped items into a schema-validated structured payload",
            move |input: SummarizeInput, _ctx| {
                let log = Arc::clone(&log_handler);
                async move {
                    log.write().push("handler:summarize_items".into());
                    let limit = if input.max_items == 0 {
                        input.items.len()
                    } else {
                        usize::try_from(input.max_items)
                            .unwrap_or(usize::MAX)
                            .min(input.items.len())
                    };
                    let slice = &input.items[..limit];
                    Ok(SummarizeOutput {
                        count: u32::try_from(slice.len()).unwrap_or(u32::MAX),
                        summary: slice.join(", "),
                        namespace: input.scope.namespace,
                    })
                }
            },
        )
        .tool_typed(
            "confirm_action",
            "Execute an inline MRTR confirmation flow using elicitation/create",
            |input: ConfirmActionInput, params, ctx| async move {
                require_client_elicitation(&ctx)?;
                if let Some(approved) = params
                    .input_responses
                    .as_ref()
                    .and_then(|m| m.get("confirm"))
                    .and_then(|v| v.get("approved"))
                    .and_then(Value::as_bool)
                {
                    let status = if approved { "confirmed" } else { "declined" };
                    return Ok(CallToolResult::structured(
                        json!({ "operation": input.operation, "status": status }),
                    )
                    .into());
                }
                let mut requests = InputRequests::new();
                requests.insert(
                    "confirm".into(),
                    mrtr_elicitation(
                        format!("Approve operation `{}`?", input.operation),
                        json!({
                            "type": "object",
                            "properties": { "approved": { "type": "boolean" } },
                            "required": ["approved"]
                        }),
                    )?,
                );
                Ok(mrtr_input_required(requests, Some(format!("op:{}", input.operation)))?.into())
            },
        )
        .task_tool_typed(
            "approval_workflow",
            "Durable task workflow pausing for MRTR operator input via TaskContext",
            |input: ApprovalTaskInput, task_ctx| async move {
                task_ctx.set_status_message(format!("awaiting approval for {}", input.ticket));
                let req = mrtr_elicitation(
                    format!("Approve ticket {}?", input.ticket),
                    json!({
                        "type": "object",
                        "properties": { "decision": { "type": "string" } },
                        "required": ["decision"]
                    }),
                )
                .map_err(TaskExit::Error)?;
                let answer = task_ctx.request_input("approve", req).await?;
                let decision = answer
                    .get("decision")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                Ok(CallToolResult::structured(
                    json!({ "ticket": input.ticket, "decision": decision }),
                ))
            },
        )
        .resource_guard(Arc::new(|uri| {
            if uri.starts_with("memo://secret/") {
                return Err(ErrorData::invalid_params(
                    "Resource guard denied secret URI",
                    None,
                ));
            }
            Ok(())
        }))
        .text_resource(
            "memo://welcome",
            "welcome",
            "text/plain",
            "Welcome to the Susi GMCP SDK reference server.",
        )
        .template(
            template,
            Arc::new(|req, _ctx| {
                Box::pin(async move {
                    let Some(id) = req.uri.strip_prefix("memo://items/") else {
                        return Err(ErrorData::resource_not_found("Template mismatch", None));
                    };
                    if id.is_empty() || id.contains('/') {
                        return Err(ErrorData::resource_not_found("Unknown memo item", None));
                    }
                    let body = json!({ "id": id, "status": "active" }).to_string();
                    Ok(text_read_response(req.uri, "application/json", body))
                })
            }),
        )
        .prompt(
            prompt,
            Arc::new(|req, _ctx| {
                Box::pin(async move {
                    let topic = req
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("topic"))
                        .and_then(Value::as_str)
                        .unwrap_or("general");
                    Ok(prompt_msg(
                        Role::User,
                        format!("Executive brief on {topic}"),
                        format!("Prepare a concise brief covering: {topic}"),
                    ))
                })
            }),
        )
        .completion(
            "draft_brief",
            Arc::new(|req, _ctx| {
                Box::pin(async move {
                    let candidates = ["architecture", "audit", "availability", "security", "sdk"];
                    let values: Vec<String> = candidates
                        .iter()
                        .filter(|c| c.starts_with(req.argument.value.as_str()))
                        .map(|s| (*s).to_string())
                        .collect();
                    let total = u32::try_from(values.len()).unwrap_or(u32::MAX);
                    Ok(CompleteResult {
                        result_type: Some(ResultType::COMPLETE),
                        completion: CompletionInfo {
                            values,
                            total: Some(total),
                            has_more: Some(false),
                        },
                        meta: None,
                    })
                })
            }),
        )
        .extension(
            "io.susi.sdk/reference",
            ExtensionStatus::Experimental,
            1,
            ext_settings,
        )
        .build()?;

    Ok((service, audit_log))
}

/// Self-contained example [`PluginBundle`] (`SDK-014..SDK-018`).
pub fn sample_plugin_bundle(
    pack_root: Option<PathBuf>,
    checksums: BTreeMap<String, String>,
    files: BTreeMap<String, String>,
) -> PluginBundle {
    let manifest = ExtensionManifest {
        id: "sdk-sample-plugin".into(),
        name: "SDK Sample Plugin".into(),
        version: "0.2.0".into(),
        api_version: "1.0".into(),
        description: "First-party SDK reference plugin fixture".into(),
        capabilities: vec!["mcp.tools.plugin_lookup".into()],
        requires: vec!["gmcp.server.v1".into(), "gmcp.tasks.v1".into()],
        optional: vec!["gmcp.telemetry.v99".into()],
        permissions: vec!["filesystem.read".into()],
        files,
        checksums,
        signatures: BTreeMap::new(),
        signer: None,
        extra: HashMap::from([("x-sdk-example".into(), json!("sample-plugin"))]),
    };

    let schema = crate::mcp::schema::root::<PluginLookupInput>();
    let lookup_tool = Tool::new_with_raw(
        "plugin_lookup",
        Some("Plugin lookup".into()),
        schema.clone(),
    );
    let watch_tool = Tool::new_with_raw(
        "plugin_watch",
        Some("Cancellable plugin task".into()),
        schema,
    );
    let prompt = make_prompt(
        "plugin_help",
        "Plugin Help",
        "Guidance contributed by sdk-sample-plugin",
        None,
    );

    let mut bundle = PluginBundle::new(manifest)
        .with_tool(
            lookup_tool,
            Arc::new(|params, _ctx| {
                Box::pin(async move {
                    let key = params
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("key"))
                        .and_then(Value::as_str)
                        .unwrap_or("default");
                    Ok(
                        CallToolResult::success(vec![ContentBlock::text(format!("plugin:{key}"))])
                            .into(),
                    )
                })
            }),
        )
        .with_task_tool(
            watch_tool,
            Arc::new(|_params, task_ctx| {
                Box::pin(async move {
                    task_ctx.cancelled().await;
                    Err(TaskExit::Cancelled)
                })
            }),
        )
        .with_resource(
            Resource::new("plugin://status", "plugin-status").with_mime_type("application/json"),
            Arc::new(|req, _ctx| {
                Box::pin(async move {
                    Ok(text_read_response(
                        req.uri,
                        "application/json",
                        json!({"plugin": "sdk-sample-plugin", "healthy": true}).to_string(),
                    ))
                })
            }),
        )
        .with_prompt(
            prompt,
            Arc::new(|_req, _ctx| {
                Box::pin(async move {
                    Ok(prompt_msg(
                        Role::Assistant,
                        "Plugin help prompt".into(),
                        "sdk-sample-plugin is active.".into(),
                    ))
                })
            }),
        )
        .with_extension(
            "io.susi.plugin/sample",
            ExtensionStatus::Experimental,
            1,
            Map::new(),
        );

    if let Some(root) = pack_root {
        bundle = bundle.with_pack_root(root);
    }
    bundle
}
