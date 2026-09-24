#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
pub mod susi_error;

// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
mod susi_paths;

// Vendored `susi-config` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
// rustfmt::skip: the file is vendored byte-identical while consumers span
// edition 2021/2024 whose style editions sort imports and indent format!
// args differently — formatting it per-crate would break the invariant.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_config;

// Vendored `susi-sandbox` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_sandbox;

// Vendored `susi_core` microkernel subset (canonical tree:
// `susi-core/vendor_template/susi_core/`): bus/registry/capture/mac state
// rendezvous with the daemon's real susi_core via `<cache>/bus/<pid>/` +
// substrate files. Allows keep the tree byte-identical across consumers:
// dead_code audits the unexercised surface; rustfmt::skip + collapsible_if
// stop edition-2024 style drift against the edition-2021 canonical source.
#[allow(dead_code, clippy::collapsible_if)]
#[rustfmt::skip]
pub mod susi_core;

// GEMI HTTP REST Substrate: OpenAI-Compatible Interface & Adaptive Web Interface
// 100% Rust implementation serving Tier 1 & Tier 2 Intelligence Swarms
//
// Async Defaults (Mandate 28): connection accept/read/write is tokio-native
// (hyper). Route handlers that ultimately invoke SusiMasterAgent (a
// synchronous, CPU-bound swarm/agent execution graph, rayon-based) dispatch
// via tokio::task::spawn_blocking rather than pretending that work is
// non-blocking — running it directly on a tokio worker thread would stall
// the reactor for every other in-flight connection.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use serde_json::json;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use susi_core::context_graph::ContextGraph;
use susi_core::plane_bus::{gawd, gemi, tools as plane_tools};

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn full_body<T: Into<Bytes>>(chunk: T) -> BoxBody {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

#[allow(clippy::result_large_err)]
fn read_json_body(body_bytes: &hyper::body::Bytes) -> Result<serde_json::Value, Response<BoxBody>> {
    match serde_json::from_slice(body_bytes) {
        Ok(v) => Ok(v),
        Err(e) => Err(api_error(
            StatusCode::BAD_REQUEST,
            &format!("JSON error: {e}"),
        )),
    }
}

fn header_str<'a>(req: &'a Request<Incoming>, name: &str) -> Option<&'a str> {
    req.headers().get(name).and_then(|v| v.to_str().ok())
}

fn json_response(status: StatusCode, payload: &serde_json::Value) -> Response<BoxBody> {
    let allow_origin = crate::susi_sandbox::manager::SusiConfig::load_global_arc()
        .unwrap_or_default()
        .get("allow_origin")
        .unwrap_or_else(|| "*".to_string());
    let origin_header =
        HeaderValue::from_str(&allow_origin).unwrap_or_else(|_| HeaderValue::from_static("*"));
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .header("Access-Control-Allow-Origin", origin_header)
        .body(full_body(
            serde_json::to_string(payload).unwrap_or_default(),
        ))
        .unwrap_or_else(|_| {
            Response::new(full_body(
                serde_json::to_string(payload).unwrap_or_default(),
            ))
        })
}

pub struct GemiServer;

impl GemiServer {
    pub fn start_http_server(workspace: PathBuf, listener: std::net::TcpListener) {
        let addr = listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        eprintln!("[GEMI REST] Substrate active on {}", addr);
        eprintln!(
            "[GEMI Web] UI Interface: http://localhost:{}/app",
            listener.local_addr().map(|a| a.port()).unwrap_or(0)
        );

        // The vendored susi_core copy owns its own ContextGraph::global() —
        // bind the same workspace JSONL log the daemon binds for its copy so
        // both sides read/write one shared event log (reads replay() it).
        ContextGraph::init_global_storage(workspace.join("context_graph.jsonl"));

        // Runs on its own daemon thread (caller wraps it in catch_unwind), so
        // a failure here only loses the GEMI HTTP surface, not the whole
        // daemon — but log clearly and return instead of panicking with a
        // raw message, since nothing retries this subsystem.
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!(
                    "[GEMI REST] Failed to start HTTP runtime: {}. GEMI HTTP is unavailable.",
                    e
                );
                return;
            }
        };

        rt.block_on(async move {
            if let Err(e) = listener.set_nonblocking(true) {
                eprintln!("[GEMI REST] Failed to set listener non-blocking: {}. GEMI HTTP is unavailable.", e);
                return;
            }
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[GEMI REST] Failed to adopt listener into tokio runtime: {}. GEMI HTTP is unavailable.", e);
                    return;
                }
            };
            let workspace = Arc::new(workspace);
            let capacity = crate::susi_sandbox::manager::SusiConfig::load_global_arc()
                .unwrap_or_default().gemi_max_concurrent_requests();
            let admission = Arc::new(tokio::sync::Semaphore::new(capacity.min(tokio::sync::Semaphore::MAX_PERMITS)));

            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("[GEMI REST] Accept error: {}", e);
                        continue;
                    }
                };
                let workspace = Arc::clone(&workspace);
                let peer_ip = peer.ip();
                let admission = Arc::clone(&admission);

                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| {
                        let workspace = Arc::clone(&workspace);
                        let admission = Arc::clone(&admission);
                        async move { handle_gemi_request(req, workspace, peer_ip, admission).await }
                    });
                    if let Err(e) = AutoBuilder::new(TokioExecutor::new())
                        .serve_connection(io, service)
                        .await
                    {
                        eprintln!("[GEMI REST] Connection error: {}", e);
                    }
                });
            }
        });
    }
}

#[allow(clippy::unwrap_used)]
async fn handle_gemi_request(
    req: Request<Incoming>,
    workspace: Arc<PathBuf>,
    peer_ip: std::net::IpAddr,
    admission: Arc<tokio::sync::Semaphore>,
) -> Result<Response<BoxBody>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    // Extract the auth material before consuming the body — the signed
    // request fields borrow the request headers.
    let authorization = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let sign_node = header_str(&req, "x-susi-node").map(str::to_string);
    let sign_ts = header_str(&req, "x-susi-req-ts").and_then(|s| s.parse().ok());
    let sign_nonce = header_str(&req, "x-susi-req-nonce").map(str::to_string);
    let sign_sig = header_str(&req, "x-susi-req-sig").map(str::to_string);
    // H9: Buffer the request body once (10MB cap) so member v2
    // signatures verify against the exact bytes the route handlers
    // parse — an on-path body substitution breaks the signature
    // instead of passing a method+path-only check.
    use http_body_util::BodyExt;
    let body_bytes = match http_body_util::Limited::new(req.into_body(), 10 * 1024 * 1024)
        .collect()
        .await
    {
        Ok(body) => body.to_bytes(),
        Err(error) => {
            let status = if error.is::<http_body_util::LengthLimitError>() {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            return Ok(api_error(status, "Unable to read request body"));
        }
    };

    // CORS preflight never carries an Authorization header, and /health is a
    // conventional unauthenticated liveness probe — everything else on this
    // world-facing surface is gated below.
    if method != Method::OPTIONS && path != "/health" {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
        let signed = susi_core::net_guard::SignedRequest {
            node: sign_node.as_deref(),
            ts_secs: sign_ts,
            nonce: sign_nonce.as_deref(),
            sig: sign_sig.as_deref(),
        };
        if !susi_core::net_guard::NetGuard::is_authorized(
            authorization.as_deref(),
            peer_ip,
            &signed,
            method.as_str(),
            &path,
            Some(&body_bytes),
        ) {
            return Ok(json_response(
                StatusCode::UNAUTHORIZED,
                &json!({"error": "Unauthorized"}),
            ));
        }
        if !susi_core::net_guard::RateLimiter::global().check(peer_ip, cfg.rate_limit_per_minute())
        {
            return Ok(json_response(
                StatusCode::TOO_MANY_REQUESTS,
                &json!({"error": "Rate limit exceeded"}),
            ));
        }
    }

    match (&method, path.as_str()) {
        (&Method::GET, "/" | "/v1" | "/v1/" | "/health" | "/app" | "/favicon.ico") => {
            let api_status = json!({
                "object": "api_status",
                "name": "SUSI OpenAI-Compatible REST Substrate",
                "version": env!("CARGO_PKG_VERSION"),
                "status": "active",
                "endpoints": [
                    "/v1/chat/completions",
                    "/v1/models",
                    "/v1/models/{id}",
                    "/v1/completions",
                    "/v1/embeddings",
                    "/context-graph/stats",
                    "/context-graph/show",
                    "/context-graph/query",
                    "/context-graph/ingest",
                    "/context-graph/compact",
                    "/telemetry",
                    "/broker/grant",
                    "/broker/request",
                    "/broker/negotiate",
                    "/broker/send",
                    "/broker/receive",
                    "/patch/apply"
                ]
            });
            Ok(json_response(StatusCode::OK, &api_status))
        }
        (&Method::GET, "/v1/models" | "/models") => {
            let ws = (*workspace).clone();
            let payload = tokio::task::spawn_blocking(
                move || json!({"object": "list", "data": openai_model_list(&ws)}),
            )
            .await
            .unwrap_or_else(|_| json!({"object": "list", "data": []}));
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::GET, p) if p.starts_with("/v1/models/") => {
            let requested = p.trim_start_matches("/v1/models/").to_owned();
            let want = requested.clone();
            let ws = (*workspace).clone();
            let payload = tokio::task::spawn_blocking(move || {
                openai_model_list(&ws).into_iter().find(|m| {
                    m.get("id").and_then(|v| v.as_str()) == Some(want.as_str())
                        || m.get("model_id").and_then(|v| v.as_str()) == Some(want.as_str())
                })
            })
            .await
            .unwrap_or_default();
            match payload {
                Some(model) => Ok(json_response(StatusCode::OK, &model)),
                None => Ok(json_response(
                    StatusCode::NOT_FOUND,
                    &json!({"error": {
                        "message": format!("The model '{requested}' does not exist"),
                        "type": "invalid_request_error",
                        "param": "model",
                        "code": "model_not_found",
                    }}),
                )),
            }
        }
        (&Method::GET, "/well-known/susi") => {
            let payload = tokio::task::spawn_blocking(move || {
                let hardware = susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
                let (engine, model) = gemi::ModelManager::get_active_engine_and_model(None);
                let tools = plane_tools::list_tools();
                let tool_names: Vec<String> = tools
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| {
                                t.get("name").and_then(|v| v.as_str()).map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "identity": "SUSI Intelligence Substrate",
                    "engine": engine,
                    "model": model,
                    "hardware": {
                        "cpus": hardware.cpus,
                        "gpu": hardware.gpu_info,
                        "acceleration": hardware.acceleration_active,
                        "os": hardware.os_info
                    },
                    "reflexes": tool_names
                })
            })
            .await
            .unwrap_or_else(|_| json!({"error": "identity introspection failed"}));
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/v1/embeddings" | "/embeddings") => {
            #[derive(serde::Deserialize)]
            struct EmbedReq {
                input: serde_json::Value,
                #[serde(default)]
                model: Option<String>,
            }
            let req: EmbedReq = match serde_json::from_slice(&body_bytes) {
                Ok(r) => r,
                Err(e) => {
                    return Ok(api_error(
                        StatusCode::BAD_REQUEST,
                        &format!("invalid embeddings request: {e}"),
                    ));
                }
            };
            // OpenAI accepts a string or an array of strings.
            let texts: Vec<String> = match &req.input {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(a) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                serde_json::Value::Null
                | serde_json::Value::Bool(_)
                | serde_json::Value::Number(_)
                | serde_json::Value::Object(_) => Vec::new(),
            };
            if texts.is_empty() {
                return Ok(api_error(
                    StatusCode::BAD_REQUEST,
                    "input must be a non-empty string or string array",
                ));
            }
            let model = req.model.clone();
            let out = tokio::task::spawn_blocking(move || {
                texts
                    .iter()
                    .map(|t| gemi::GemiEngine::embed(t, model.as_deref()))
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            if out.iter().any(Option::is_none) {
                return Ok(api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no embedding-capable provider available",
                ));
            }
            let data: Vec<serde_json::Value> = out
                .into_iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    v.map(|e| json!({"object": "embedding", "index": i, "embedding": e}))
                })
                .collect();
            let payload = json!({
                "object": "list",
                "data": data,
                "model": req.model.unwrap_or_else(|| "susi-embed".to_string()),
                "usage": {"prompt_tokens": 0, "total_tokens": 0},
            });
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, p)
            if matches!(
                p,
                "/v1/chat/completions" | "/chat/completions" | "/v1/completions"
            ) || p == "/"
                || p == "/v1"
                || p == "/v1/" =>
        {
            // The body was already buffered (bounded) for signature
            // verification — the handler sees the same bytes that signed.
            let completion = match parse_completion(&body_bytes, path == "/v1/completions") {
                Ok(completion) => completion,
                Err(message) => return Ok(api_error(StatusCode::BAD_REQUEST, &message)),
            };
            // OpenAI contract: an unknown `model` is a 400, not a silent
            // reroute. Known names are honored by the governed pipeline's
            // own selection (the response labels the model that served).
            if let Some(ref requested) = completion.model {
                let known = gemi::ModelManager::list_models(&workspace)
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                m.get("model_id")
                                    .or_else(|| m.get("id"))
                                    .and_then(|v| v.as_str())
                            })
                            .any(|id| {
                                // Accept the full id, the basename, or the
                                // file stem — /v1/models ids are paths for
                                // locally scanned GGUF weights.
                                let stem = std::path::Path::new(id)
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or(id);
                                id == requested
                                    || stem == requested
                                    || std::path::Path::new(id)
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        == Some(requested.as_str())
                            })
                    })
                    .unwrap_or(false);
                if !known {
                    return Ok(api_error(
                        StatusCode::BAD_REQUEST,
                        &format!("unknown model '{requested}' — see /v1/models"),
                    ));
                }
            }
            let permit = match admission.try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    let mut response = json_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        &json!({"error": {"message": "GEMI is at capacity; retry later", "type": "server_error", "code": "server_busy"}}),
                    );
                    response
                        .headers_mut()
                        .insert("Retry-After", HeaderValue::from_static("1"));
                    return Ok(response);
                }
            };
            let is_streaming = completion.stream;
            let pulse_intent = completion.prompt;
            let intent = gemi::IntentClassifier::classify(&pulse_intent);
            // A caller-requested model labels the response; intent
            // classification only applies when no model was named. The
            // label is the display id (stem), matching /v1/models.
            let resolved_model = completion.model.clone().unwrap_or_else(|| {
                gemi::ModelManager::get_active_engine_and_model(Some(&intent)).1
            });
            let active_model = model_display_id(&resolved_model).to_string();
            crate::susi_sandbox::manager::SusiAuditLogger::log_event(
                &workspace,
                "WEB_MISSION_START",
                &pulse_intent,
            );
            let trimmed_prompt = pulse_intent.trim().to_string();

            if is_streaming {
                Ok(build_streaming_response(
                    trimmed_prompt,
                    active_model,
                    completion.model.clone(),
                    Arc::clone(&workspace),
                    path == "/v1/completions",
                    permit,
                ))
            } else {
                let ws = (*workspace).clone();
                let prompt_for_task = trimmed_prompt.clone();
                let model_for_task = completion.model.clone();
                let content = match tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let final_resp = gawd::solve_mission_with_model(
                        &prompt_for_task,
                        &ws,
                        env!("CARGO_PKG_VERSION"),
                        model_for_task.as_deref(),
                    );
                    crate::susi_sandbox::manager::SusiMemory::save_interaction(
                        &ws,
                        &prompt_for_task,
                        &final_resp,
                        env!("CARGO_PKG_VERSION"),
                    );
                    final_resp
                })
                .await
                {
                    Ok(content) => content,
                    Err(error) => {
                        tracing::error!(%error, "GEMI inference task failed");
                        return Ok(json_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &json!({"error": {"message": "Inference task failed", "type": "server_error"}}),
                        ));
                    }
                };

                let payload =
                    completion_response(&active_model, &content, path == "/v1/completions");
                Ok(json_response(StatusCode::OK, &payload))
            }
        }
        (&Method::OPTIONS, _) => {
            let cfg =
                crate::susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
            let allow_origin = cfg.get("allow_origin").unwrap_or_else(|| "*".to_string());
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(
                    "Access-Control-Allow-Origin",
                    HeaderValue::from_str(&allow_origin)
                        .unwrap_or_else(|_| HeaderValue::from_static("*")),
                )
                .header(
                    "Access-Control-Allow-Methods",
                    HeaderValue::from_static("GET, POST, OPTIONS"),
                )
                .header(
                    "Access-Control-Allow-Headers",
                    HeaderValue::from_str(&allow_origin)
                        .unwrap_or_else(|_| HeaderValue::from_static("*")),
                )
                .body(full_body(Vec::new()))
                .unwrap())
        }
        (&Method::GET, "/context-graph/stats") => {
            let payload = tokio::task::spawn_blocking(move || {
                let graph = ContextGraph::global();
                let _ = graph.replay();
                serde_json::to_value(graph.stats())
                    .unwrap_or_else(|_| serde_json::json!({"error": "serialization failed"}))
            })
            .await
            .unwrap_or_else(|_| serde_json::json!({"error": "stats failed"}));
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::GET, "/context-graph/show") => {
            let ws = (*workspace).clone();
            let payload = tokio::task::spawn_blocking(move || {
                let graph = ContextGraph::global();
                let _ = graph.replay();
                let subgraph = graph.workspace_subgraph(&ws);
                let events: Vec<_> = subgraph
                    .nodes
                    .into_iter()
                    .map(|n| {
                        serde_json::json!({
                            "type": "node",
                            "id": n.id.0,
                            "kind": format!("{:?}", n.kind),
                            "label": n.label,
                            "created_at": n.created_at,
                        })
                    })
                    .chain(subgraph.edges.into_iter().map(|e| {
                        serde_json::json!({
                            "type": "edge",
                            "id": e.id,
                            "source": e.source.0,
                            "target": e.target.0,
                            "kind": format!("{:?}", e.kind),
                            "created_at": e.created_at,
                        })
                    }))
                    .collect();
                serde_json::json!({
                    "workspace": ws.display().to_string(),
                    "event_count": events.len(),
                    "events": events,
                })
            })
            .await
            .unwrap_or_else(|_| serde_json::json!({"error": "show failed"}));
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/context-graph/query") => {
            let ws = (*workspace).clone();
            let payload = match read_json_body(&body_bytes) {
                Ok(body) => tokio::task::spawn_blocking(move || {
                    let graph = ContextGraph::global();
                    let _ = graph.replay();
                    // An unbounded workspace subgraph can run to megabytes of
                    // ambient file-change noise — callers that need the full
                    // graph raise `limit` explicitly (0 = no cap).
                    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
                    let mut subgraph =
                        if let Some(node_id) = body.get("node_id").and_then(|v| v.as_str()) {
                            let depth =
                                body.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
                            let node = susi_core::context_graph::NodeId(node_id.to_string());
                            graph.related(&node, depth)
                        } else {
                            graph.workspace_subgraph(&ws)
                        };
                    let truncated = limit > 0 && subgraph.nodes.len() > limit;
                    if truncated {
                        subgraph.nodes.truncate(limit);
                        let keep: std::collections::HashSet<_> =
                            subgraph.nodes.iter().map(|n| n.id.clone()).collect();
                        subgraph
                            .edges
                            .retain(|e| keep.contains(&e.source) && keep.contains(&e.target));
                    }
                    let mut value = serde_json::to_value(&subgraph)
                        .unwrap_or_else(|_| serde_json::json!({"error": "serialization failed"}));
                    if truncated && let Some(obj) = value.as_object_mut() {
                        obj.insert(String::from("truncated"), serde_json::Value::Bool(true));
                    }
                    value
                })
                .await
                .unwrap_or_else(|_| serde_json::json!({"error": "query failed"})),
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/context-graph/ingest") => {
            let ws = (*workspace).clone();
            let payload = match read_json_body(&body_bytes) {
                Ok(body) => tokio::task::spawn_blocking(move || {
                    let graph = ContextGraph::global();
                    let _ = graph.replay();
                    let source = body["source"].as_str().unwrap_or("unknown");
                    let label = body["label"].as_str().unwrap_or("external context");
                    let payload = body.get("payload").unwrap_or(&serde_json::Value::Null);
                    let ws_path = body
                        .get("workspace")
                        .and_then(|v| v.as_str())
                        .map(std::path::PathBuf::from)
                        .unwrap_or(ws);
                    let user = body.get("user").and_then(|v| v.as_str());
                    let id =
                        graph.record_external_context(source, label, payload, Some(&ws_path), user);
                    serde_json::json!({"ingested": true, "node_id": id.0})
                })
                .await
                .unwrap_or_else(|_| serde_json::json!({"error": "ingest failed"})),
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/context-graph/compact") => {
            let payload = tokio::task::spawn_blocking(move || {
                let graph = ContextGraph::global();
                let _ = graph.replay();
                match graph.compact() {
                    Ok((old_lines, new_lines)) => {
                        serde_json::json!({"compacted": true, "old_lines": old_lines, "new_lines": new_lines})
                    }
                    Err(e) => serde_json::json!({"error": e.to_string()}),
                }
            })
            .await
            .unwrap_or_else(|_| serde_json::json!({"error": "compact failed"}));
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::GET, "/telemetry") => {
            let ws = (*workspace).clone();
            let payload = tokio::task::spawn_blocking(move || {
                let snapshot = susi_core::plane_bus::gemi::sample_telemetry();
                if let Ok(snap) =
                    serde_json::from_value::<susi_core::TelemetrySnapshot>(snapshot.clone())
                {
                    ContextGraph::global().record_telemetry(&snap, Some(&ws));
                }
                snapshot
            })
            .await
            .unwrap_or_else(|_| json!({"error": "telemetry failed"}));
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/broker/grant") => {
            let ws = (*workspace).clone();
            let payload = match read_json_body(&body_bytes) {
                Ok(body) => tokio::task::spawn_blocking(move || {
                    use susi_core::broker::{IpcBroker, PermissionScope};
                    let grantor = body["grantor"].as_str().unwrap_or("susi");
                    let Some(grantee) = body["grantee"].as_str() else {
                        return json!({"error": "grantee required"});
                    };
                    let Some(resource) = body["resource"].as_str() else {
                        return json!({"error": "resource required"});
                    };
                    let Some(action) = body["action"].as_str() else {
                        return json!({"error": "action required"});
                    };
                    let ttl_secs = body.get("ttl_secs").and_then(|v| v.as_u64());
                    let grant = IpcBroker::global().grant_and_record(
                        grantor,
                        grantee,
                        PermissionScope::new(resource, action),
                        ttl_secs,
                        Some(&ws),
                    );
                    serde_json::to_value(grant).unwrap_or_else(|_| json!({"error": "serialize"}))
                })
                .await
                .unwrap_or_else(|_| json!({"error": "grant failed"})),
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/broker/request") => {
            let payload = match read_json_body(&body_bytes) {
                Ok(body) => tokio::task::spawn_blocking(move || {
                    use susi_core::broker::{IpcBroker, PermissionScope};
                    let Some(requester) = body["requester"].as_str() else {
                        return json!({"error": "requester required"});
                    };
                    let grantor = body["grantor"].as_str().unwrap_or("susi");
                    let Some(resource) = body["resource"].as_str() else {
                        return json!({"error": "resource required"});
                    };
                    let Some(action) = body["action"].as_str() else {
                        return json!({"error": "action required"});
                    };
                    let ttl_secs = body.get("ttl_secs").and_then(|v| v.as_u64());
                    let req = IpcBroker::global().request(
                        requester,
                        grantor,
                        PermissionScope::new(resource, action),
                        ttl_secs,
                    );
                    serde_json::to_value(req).unwrap_or_else(|_| json!({"error": "serialize"}))
                })
                .await
                .unwrap_or_else(|_| json!({"error": "request failed"})),
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/broker/negotiate") => {
            let ws = (*workspace).clone();
            let payload = match read_json_body(&body_bytes) {
                Ok(body) => tokio::task::spawn_blocking(move || {
                    use susi_core::broker::IpcBroker;
                    let Some(request_id) = body["request_id"].as_str() else {
                        return json!({"error": "request_id required"});
                    };
                    let actor = body["actor"].as_str().unwrap_or("susi");
                    let approve = body["approve"].as_bool().unwrap_or(false);
                    match IpcBroker::global().negotiate(request_id, actor, approve, Some(&ws)) {
                        Ok(resolved) => serde_json::to_value(resolved)
                            .unwrap_or_else(|_| json!({"error": "serialize"})),
                        Err(e) => json!({"error": e}),
                    }
                })
                .await
                .unwrap_or_else(|_| json!({"error": "negotiate failed"})),
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/broker/send") => {
            let payload = match read_json_body(&body_bytes) {
                Ok(body) => tokio::task::spawn_blocking(move || {
                    use susi_core::broker::IpcBroker;
                    let Some(from) = body["from"].as_str() else {
                        return json!({"error": "from required"});
                    };
                    let Some(to) = body["to"].as_str() else {
                        return json!({"error": "to required"});
                    };
                    let topic = body["topic"].as_str().unwrap_or("message");
                    let msg_payload = body
                        .get("payload")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    match IpcBroker::global().send(from, to, topic, msg_payload) {
                        Ok(msg) => serde_json::to_value(msg)
                            .unwrap_or_else(|_| json!({"error": "serialize"})),
                        Err(e) => json!({"error": e}),
                    }
                })
                .await
                .unwrap_or_else(|_| json!({"error": "send failed"})),
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/broker/receive") => {
            let payload = match read_json_body(&body_bytes) {
                Ok(body) => tokio::task::spawn_blocking(move || {
                    use susi_core::broker::IpcBroker;
                    let Some(recipient) = body["recipient"].as_str() else {
                        return json!({"error": "recipient required"});
                    };
                    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                    let msgs = IpcBroker::global().receive(recipient, limit);
                    json!({"recipient": recipient, "messages": msgs})
                })
                .await
                .unwrap_or_else(|_| json!({"error": "receive failed"})),
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, "/patch/apply") => {
            let ws = (*workspace).clone();
            let payload = match read_json_body(&body_bytes) {
                Ok(body) => tokio::task::spawn_blocking(move || {
                    let cfg = crate::susi_sandbox::manager::SusiConfig::load_global_arc()
                        .unwrap_or_default();
                    let mut payload = body;
                    if let Some(obj) = payload.as_object_mut() {
                        obj.insert("workspace".into(), json!(ws.display().to_string()));
                        obj.insert("trust_level".into(), json!(cfg.trust_level()));
                    }
                    match gawd::apply_patch(payload) {
                        Ok(outcome) => outcome,
                        Err(e) => json!({"error": e}),
                    }
                })
                .await
                .unwrap_or_else(|_| json!({"error": "patch failed"})),
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &payload))
        }
        _ => Ok(json_response(
            StatusCode::NOT_FOUND,
            &json!({"error": "Endpoint not found"}),
        )),
    }
}

#[allow(clippy::unwrap_used)]
#[allow(clippy::too_many_arguments)] // response constructor: the two
// model strings serve different roles (response label vs routing hint)
// and the permit must travel with the request — a struct adds a type
// for one call site.
fn build_streaming_response(
    prompt: String,
    model_name: String,
    requested_model: Option<String>,
    workspace: Arc<PathBuf>,
    legacy: bool,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Response<BoxBody> {
    let rx = completion_stream(model_name, legacy, move |callback| {
        let _permit = permit;
        gemi::GemiEngine::generate_reasoning_stream_with_model(
            &prompt,
            &workspace,
            &|chunk| {
                callback(chunk);
            },
            requested_model.as_deref(),
        )
    });
    let stream =
        ReceiverStream::new(rx).map(|chunk| Ok::<_, Infallible>(Frame::data(Bytes::from(chunk))));
    let body = StreamBody::new(stream).boxed();

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .header(
            "Access-Control-Allow-Origin",
            HeaderValue::from_str(
                &crate::susi_sandbox::manager::SusiConfig::load_global_arc()
                    .unwrap_or_default()
                    .get("allow_origin")
                    .unwrap_or_else(|| "*".to_string()),
            )
            .unwrap_or_else(|_| HeaderValue::from_static("*")),
        )
        .body(body)
        .unwrap()
}

fn completion_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "susi-{}-{}-{}",
        std::process::id(),
        now_secs(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// OpenAI-facing model id: file stem for path-like internal ids,
/// the id itself otherwise. `/v1/models` and completion labels share
/// this so a client can echo `id` back into `model` verbatim.
fn model_display_id(id: &str) -> &str {
    if id.contains('/') {
        std::path::Path::new(id)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(id)
    } else {
        id
    }
}

/// `/v1/models` entries in OpenAI shape. `id` is the display id a client
/// echoes back as `model`; the raw internal id stays as `model_id`.
fn openai_model_list(workspace: &std::path::Path) -> Vec<serde_json::Value> {
    let models = gemi::ModelManager::list_models(workspace);
    models
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let id = m
                        .get("model_id")
                        .or_else(|| m.get("id"))
                        .and_then(|v| v.as_str())?;
                    Some(json!({
                        "id": model_display_id(id),
                        "model_id": id,
                        "object": "model",
                        "owned_by": "susi",
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn completion_response(model: &str, content: &str, legacy: bool) -> serde_json::Value {
    let choice = if legacy {
        json!({"index": 0, "text": content, "finish_reason": "stop", "logprobs": null})
    } else {
        json!({"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"})
    };
    json!({"id": completion_id(), "object": if legacy { "text_completion" } else { "chat.completion" },
        "created": now_secs(), "model": model, "choices": [choice]})
}

fn stream_chunk(
    identity: (&str, u64),
    model: &str,
    legacy: bool,
    delta: serde_json::Value,
    finished: bool,
) -> String {
    let (id, created) = identity;
    let reason = if finished {
        json!("stop")
    } else {
        serde_json::Value::Null
    };
    let choice = if legacy {
        json!({"index": 0, "text": delta.get("content").and_then(|v| v.as_str()).unwrap_or(""),
            "finish_reason": reason, "logprobs": null})
    } else {
        json!({"index": 0, "delta": delta, "finish_reason": reason})
    };
    format!(
        "data: {}\n\n",
        json!({"id": id, "created": created, "model": model,
        "object": if legacy { "text_completion" } else { "chat.completion.chunk" }, "choices": [choice]})
    )
}

fn completion_stream(
    model: String,
    legacy: bool,
    solve: impl FnOnce(&dyn Fn(String)) -> String + Send + 'static,
) -> tokio::sync::mpsc::Receiver<String> {
    // Bound queued frames and split large solver outputs so a slow client cannot
    // retain an unbounded response queue. Blocking sends run only on the blocking pool.
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let error_tx = tx.clone();
    let task = tokio::task::spawn_blocking(move || {
        let id = completion_id();
        let created = now_secs();
        if tx
            .blocking_send(stream_chunk(
                (&id, created),
                &model,
                legacy,
                json!({"role": "assistant"}),
                false,
            ))
            .is_err()
        {
            return;
        }
        let emitted = std::cell::Cell::new(false);
        let callback = |piece: String| {
            if piece.is_empty() {
                return;
            }
            emitted.set(true);
            let mut remaining = piece.as_str();
            while !remaining.is_empty() {
                let mut end = remaining.len().min(4096);
                while !remaining.is_char_boundary(end) {
                    end -= 1;
                }
                let (chunk, rest) = remaining.split_at(end);
                if tx
                    .blocking_send(stream_chunk(
                        (&id, created),
                        &model,
                        legacy,
                        json!({"content": chunk}),
                        false,
                    ))
                    .is_err()
                {
                    return;
                }
                remaining = rest;
            }
        };
        let result = solve(&callback);
        // Some solver paths return a complete answer without invoking callbacks.
        if !emitted.get() {
            callback(result);
        }
        if tx
            .blocking_send(stream_chunk(
                (&id, created),
                &model,
                legacy,
                json!({}),
                true,
            ))
            .is_ok()
        {
            let _ = tx.blocking_send("data: [DONE]\n\n".to_owned());
        }
    });
    tokio::spawn(async move {
        if let Err(error) = task.await {
            tracing::error!(%error, "GEMI streaming task failed");
            let _ = error_tx
                .send(format!(
                    "data: {}\n\n",
                    json!({"error": {
                        "message": "Inference task failed", "type": "server_error"
                    }})
                ))
                .await;
            let _ = error_tx.send("data: [DONE]\n\n".to_owned()).await;
        }
    });
    rx
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn api_error(status: StatusCode, message: &str) -> Response<BoxBody> {
    json_response(
        status,
        &json!({"error": {
            "message": message, "type": "invalid_request_error", "param": null, "code": null
        }}),
    )
}

struct CompletionInput {
    prompt: String,
    stream: bool,
    model: Option<String>,
}

fn parse_completion(body: &[u8], legacy: bool) -> Result<CompletionInput, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {e}"))?;
    let object = value.as_object().ok_or("Request must be a JSON object")?;
    let stream = match object.get("stream") {
        None => false,
        Some(value) => value.as_bool().ok_or("stream must be a boolean")?,
    };
    let prompt = if legacy {
        object
            .get("prompt")
            .and_then(|p| p.as_str())
            .ok_or("prompt must be a non-empty string")?
            .to_owned()
    } else {
        let messages = object
            .get("messages")
            .and_then(|m| m.as_array())
            .filter(|m| !m.is_empty())
            .ok_or("messages must be a non-empty array")?;
        let mut turns = Vec::with_capacity(messages.len());
        for message in messages {
            let role = message
                .get("role")
                .and_then(|r| r.as_str())
                .filter(|r| matches!(*r, "system" | "developer" | "user" | "assistant"))
                .ok_or(
                    "Each message must have a supported role: system, developer, user, assistant",
                )?;
            let content = match message.get("content") {
                Some(serde_json::Value::String(text)) => text.clone(),
                Some(serde_json::Value::Array(parts)) if !parts.is_empty() => {
                    let mut text = String::new();
                    for part in parts {
                        if part.get("type").and_then(|t| t.as_str()) != Some("text") {
                            return Err("Only text content parts are supported".into());
                        }
                        text.push_str(
                            part.get("text")
                                .and_then(|t| t.as_str())
                                .ok_or("Text content parts require a text string")?,
                        );
                    }
                    text
                }
                _ => return Err("Each message requires string or text-part content".into()),
            };
            if content.trim().is_empty() {
                return Err("Message content must not be empty".into());
            }
            turns.push((role, content));
        }
        // Preserve the direct single-user task path; carry all roles and history
        // for conversations through the existing text-based agent interface.
        if turns.len() == 1 && turns[0].0 == "user" {
            turns.remove(0).1
        } else {
            turns
                .into_iter()
                .map(|(role, content)| format!("{role}: {content}"))
                .collect::<Vec<_>>()
                .join("\n\n")
        }
    };
    if prompt.trim().is_empty() {
        return Err("Prompt must not be empty".into());
    }
    let model = object
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string);
    Ok(CompletionInput {
        prompt,
        stream,
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_conversation_and_all_text_parts() {
        let input = parse_completion(br#"{"messages":[{"role":"system","content":"Be brief"},{"role":"user","content":"Remember 42"},{"role":"assistant","content":"OK"},{"role":"user","content":[{"type":"text","text":"What "},{"type":"text","text":"number?"}]}],"stream": true}"#, false).unwrap();
        assert_eq!(
            input.prompt,
            "system: Be brief\n\nuser: Remember 42\n\nassistant: OK\n\nuser: What number?"
        );
        assert!(input.stream);
    }

    #[test]
    fn parses_stream_as_json_boolean() {
        let input = parse_completion(b"{\"prompt\":\"hello\",\"stream\":\ntrue}", true).unwrap();
        assert!(input.stream);
        assert_eq!(input.prompt, "hello");
        assert!(parse_completion(br#"{"prompt":"hello","stream":"true"}"#, true).is_err());
    }

    #[test]
    fn invalid_requests_never_become_default_tasks() {
        for body in [
            "",
            "not json",
            "null",
            "{}",
            "{\"messages\":[]}",
            r#"{"messages":[{"role":"user","content":" "}]}"#,
            r#"{"messages":[{"role":"user","content":[{"type":"image_url","image_url":"x"}]}]}"#,
        ] {
            assert!(parse_completion(body.as_bytes(), false).is_err(), "{body}");
        }
        assert!(parse_completion(br#"{"prompt":" "}"#, true).is_err());
    }
    #[tokio::test]
    async fn streaming_delivers_returned_answer_and_escapes_model_names() {
        let mut rx = completion_stream("quoted\"model".into(), false, |_| "Hello 🦀".into());
        let mut content = String::new();
        let mut stopped = false;
        while let Some(frame) = rx.recv().await {
            if frame == "data: [DONE]\n\n" {
                assert!(stopped);
                break;
            }
            let value: serde_json::Value =
                serde_json::from_str(frame.strip_prefix("data: ").unwrap().trim()).unwrap();
            assert_eq!(value["model"], "quoted\"model");
            if let Some(piece) = value["choices"][0]["delta"]["content"].as_str() {
                content.push_str(piece);
            }
            stopped |= value["choices"][0]["finish_reason"] == "stop";
        }
        assert_eq!(content, "Hello 🦀");
        assert!(stopped);
    }

    #[tokio::test]
    async fn streaming_does_not_duplicate_callback_output_and_chunks_unicode() {
        let answer = "🦀".repeat(3000);
        let expected = answer.clone();
        let mut rx = completion_stream("model".into(), true, move |callback| {
            callback(answer.clone());
            answer
        });
        let mut content = String::new();
        while let Some(frame) = rx.recv().await {
            if frame == "data: [DONE]\n\n" {
                break;
            }
            assert!(frame.len() < 5000);
            let value: serde_json::Value = serde_json::from_str(frame[6..].trim()).unwrap();
            assert_eq!(value["object"], "text_completion");
            content.push_str(value["choices"][0]["text"].as_str().unwrap());
        }
        assert_eq!(content, expected);
    }

    #[test]
    fn completion_metadata_is_current_unique_and_route_specific() {
        let first = completion_response("m", "answer", true);
        let second = completion_response("m", "answer", false);
        assert_ne!(first["id"], second["id"]);
        assert!(first["created"].as_u64().unwrap().abs_diff(now_secs()) <= 1);
        assert_eq!(first["choices"][0]["text"], "answer");
        assert_eq!(second["choices"][0]["message"]["content"], "answer");
    }
    #[tokio::test]
    async fn disconnected_stream_releases_admission() {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = admission.clone().try_acquire_owned().unwrap();
        let rx = completion_stream("m".into(), false, move |_| {
            let _permit = permit;
            "answer".into()
        });
        drop(rx);
        let permit = tokio::time::timeout(std::time::Duration::from_secs(5), admission.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        // If the worker raced with disconnect it may already have started;
        // either way the permit must be released without a reader draining SSE.
        assert_eq!(admission.available_permits(), 1);
    }

    #[tokio::test]
    async fn slow_stream_retains_admission_until_work_finishes() {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = admission.clone().try_acquire_owned().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let rx = completion_stream("m".into(), false, move |callback| {
            let _permit = permit;
            let _ = started_tx.send(());
            callback("x".repeat(4096 * 32));
            "".into()
        });
        started_rx.await.unwrap();
        assert!(admission.clone().try_acquire_owned().is_err());
        drop(rx);
        let permit = tokio::time::timeout(std::time::Duration::from_secs(5), admission.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        assert_eq!(admission.available_permits(), 1);
    }
}
