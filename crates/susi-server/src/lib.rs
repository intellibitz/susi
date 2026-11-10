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

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod anthropic_gateway;
pub mod prometheus_metrics;

// GEMI HTTP REST Substrate: the OpenAI-compatible inference socket (plus the
// Anthropic Messages translation and a Prometheus scrape). Control-plane
// operations (runtime models, placement, provider recovery) are MCP tools on
// the standards socket, not ad-hoc routes here.
// 100% Rust implementation serving Tier 1 & Tier 2 Intelligence Swarms
//
// Async Defaults (Mandate 28): connection accept/read/write is tokio-native
// (hyper). Route handlers that ultimately invoke SusiMasterAgent (a
// synchronous, CPU-bound swarm/agent execution graph on std scoped worker
// threads, `susi_core::parallel`) dispatch
// via tokio::task::spawn_blocking rather than pretending that work is
// non-blocking — running it directly on a tokio worker thread would stall
// the reactor for every other in-flight connection.

use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::json;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use susi_http_transport::http_body::{self as http_body_util, BodyExt, Full};
use susi_http_transport::http_conn::TokioIo;

use susi_core::context_graph::ContextGraph;
use susi_core::plane_bus::{gawd, gemi, tools as plane_tools};

// Dual-protocol transport: the accept loop sniffs each connection's first
// byte. A TLS ClientHello (0x16) is served over TLS when an acceptor is
// configured; anything else is plain HTTP — internal `http://127.0.0.1`
// callers are unaffected. `require_tls_remote` drops non-TLS bytes from
// off-host peers; loopback plaintext is always allowed.
use susi_http_transport::dual_transport;
use susi_http_transport::http_conn;

/// Concurrent GEMI REST connections; excess connections are closed at accept.
const MAX_CONNECTIONS: usize = 512;
use dual_transport::negotiate_transport;
use tokio_rustls::TlsAcceptor;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

struct MpscBody(tokio::sync::mpsc::Receiver<String>);

impl hyper::body::Body for MpscBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.0
            .poll_recv(cx)
            .map(|opt| opt.map(|chunk| Ok(Frame::data(Bytes::from(chunk)))))
    }
}

fn full_body<T: Into<Bytes>>(chunk: T) -> BoxBody {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

#[allow(clippy::result_large_err)] // the Err is the ready-to-send HTTP response; boxing it gains nothing on this cold path
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
    body_response(
        status,
        "application/json",
        serde_json::to_string(payload).unwrap_or_default(),
    )
}

/// A complete response of `content_type`, carrying the configured CORS origin.
fn body_response(
    status: StatusCode,
    content_type: &'static str,
    body: String,
) -> Response<BoxBody> {
    let allow_origin = crate::susi_sandbox::manager::SusiConfig::load_global_arc()
        .unwrap_or_default()
        .get("allow_origin")
        .unwrap_or_else(|| "*".to_string());
    let origin_header =
        HeaderValue::from_str(&allow_origin).unwrap_or_else(|_| HeaderValue::from_static("*"));
    let fallback = body.clone();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static(content_type))
        .header("Access-Control-Allow-Origin", origin_header)
        .body(full_body(body))
        .unwrap_or_else(|_| Response::new(full_body(fallback)))
}

pub struct GemiServer;

impl GemiServer {
    pub fn start_http_server(
        workspace: PathBuf,
        listeners: Vec<std::net::TcpListener>,
        tls: Option<TlsAcceptor>,
        require_tls_remote: bool,
    ) {
        for listener in &listeners {
            let addr = listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default();
            eprintln!("[GEMI REST] Substrate active on {}", addr);
        }

        // `ContextGraph::global()` is per-process — bind the same workspace
        // JSONL log the daemon binds so both processes read/write one shared
        // event log (reads replay() it).
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
            let workspace = Arc::new(workspace);
            let capacity = crate::susi_sandbox::manager::SusiConfig::load_global_arc()
                .unwrap_or_default().gemi_max_concurrent_requests();
            let admission = Arc::new(tokio::sync::Semaphore::new(capacity.min(tokio::sync::Semaphore::MAX_PERMITS)));
            // Open connections are capped separately from completion
            // admission: an idle keep-alive or slow client costs an fd even
            // when it never asks for inference.
            let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));

            // One accept loop per bound socket (loopback + external when the
            // bind address is a specific non-loopback IP).
            let mut accept_loops = Vec::new();
            for std_listener in listeners {
                if let Err(e) = std_listener.set_nonblocking(true) {
                    eprintln!("[GEMI REST] Failed to set listener non-blocking: {}. Socket dropped.", e);
                    continue;
                }
                let listener = match tokio::net::TcpListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("[GEMI REST] Failed to adopt listener into tokio runtime: {}. Socket dropped.", e);
                        continue;
                    }
                };
                let workspace = Arc::clone(&workspace);
                let admission = Arc::clone(&admission);
                let connections = Arc::clone(&connections);
                let tls = tls.clone();
                accept_loops.push(tokio::spawn(async move {
                    loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("[GEMI REST] Accept error: {}", e);
                        continue;
                    }
                };
                let Ok(connection) = Arc::clone(&connections).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let workspace = Arc::clone(&workspace);
                let peer_ip = peer.ip();
                let admission = Arc::clone(&admission);
                let tls = tls.clone();

                tokio::spawn(async move {
                    let _connection = connection;
                    // host_only serves this machine alone, whatever the bind.
                    if crate::susi_core::mac_policy::refuses_remote_client(peer_ip) {
                        return;
                    }
                    let remote = !peer_ip.to_canonical().is_loopback();
                    let Some(io) =
                        negotiate_transport(stream, remote, tls.as_ref(), require_tls_remote, "[GEMI REST]").await
                    else {
                        return;
                    };
                    let io = TokioIo::new(io);
                    let service = service_fn(move |req| {
                        let workspace = Arc::clone(&workspace);
                        let admission = Arc::clone(&admission);
                        async move { handle_gemi_request(req, workspace, peer_ip, admission).await }
                    });
                    if let Err(e) = http_conn::connection_builder()
                        .serve_connection(io, service)
                        .await
                    {
                        eprintln!("[GEMI REST] Connection error: {}", e);
                    }
                });
            }
                }));
            }
            for accept_loop in accept_loops {
                let _ = accept_loop.await;
            }
        });
    }
}

fn explicit_model_conflict(
    model: &str,
    is_cloud: bool,
    requires: Option<&str>,
    max_cost: Option<f64>,
    allow_cloud: bool,
) -> Option<&'static str> {
    if is_cloud && !allow_cloud {
        return Some("susi.allow_cloud=false conflicts with the requested cloud model");
    }
    if !is_cloud && requires.is_some_and(|value| value.eq_ignore_ascii_case("vision")) {
        return Some("the requested local model cannot satisfy susi.requires=vision");
    }
    if is_cloud && max_cost == Some(0.0) && !model.to_ascii_lowercase().contains("free") {
        return Some("the requested cloud model is not an explicitly free route");
    }
    None
}

/// The credential a request presents: its `Authorization` header, or, on
/// the Anthropic Messages route, the `x-api-key` header Anthropic SDKs send,
/// as the same host bearer.
fn presented_credential(headers: &hyper::HeaderMap, path: &str) -> Option<String> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    header(hyper::header::AUTHORIZATION.as_str())
        .map(str::to_string)
        .or_else(|| {
            (path == "/v1/messages")
                .then(|| header("x-api-key"))
                .flatten()
                .map(|key| format!("Bearer {key}"))
        })
}

/// A rejection made before routing (body, auth, rate limit), in the error
/// envelope the route's clients parse: Anthropic's on `/v1/messages`,
/// OpenAI's everywhere else.
fn pre_dispatch_error(path: &str, status: StatusCode, message: &str) -> Response<BoxBody> {
    if path == "/v1/messages" {
        anthropic_error(status, message)
    } else {
        api_error(status, message)
    }
}

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
    let authorization = presented_credential(req.headers(), &path);
    let sign_node = header_str(&req, "x-susi-node").map(str::to_string);
    let sign_ts = header_str(&req, "x-susi-req-ts").and_then(|s| s.parse().ok());
    let sign_nonce = header_str(&req, "x-susi-req-nonce").map(str::to_string);
    let sign_sig = header_str(&req, "x-susi-req-sig").map(str::to_string);
    // H9: Buffer the request body once (10MB cap) so member v2
    // signatures verify against the exact bytes the route handlers
    // parse — an on-path body substitution breaks the signature
    // instead of passing a method+path-only check.
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
            return Ok(pre_dispatch_error(
                &path,
                status,
                "Unable to read request body",
            ));
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
            return Ok(pre_dispatch_error(
                &path,
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
            ));
        }
        if !susi_core::net_guard::RateLimiter::global().check_unless_local_host(
            authorization.as_deref(),
            peer_ip,
            cfg.rate_limit_per_minute(),
        ) {
            return Ok(pre_dispatch_error(
                &path,
                StatusCode::TOO_MANY_REQUESTS,
                "Rate limit exceeded",
            ));
        }
    }

    if method == Method::POST && path == "/v1/messages" {
        return Ok(anthropic_messages(&body_bytes, workspace, admission).await);
    }
    dispatch_gemi_route(method, path, body_bytes, workspace, admission).await
}

/// Route one authenticated request. Split from [`handle_gemi_request`] so the
/// Anthropic Messages translation can run a chat completion through the very
/// same handler the OpenAI-compatible route uses.
async fn dispatch_gemi_route(
    method: Method,
    path: String,
    body_bytes: Bytes,
    workspace: Arc<PathBuf>,
    admission: Arc<tokio::sync::Semaphore>,
) -> Result<Response<BoxBody>, Infallible> {
    match (&method, path.as_str()) {
        (&Method::GET, "/" | "/v1" | "/v1/" | "/health") => {
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
                    "/v1/messages",
                    "/metrics",
                    "/.well-known/susi",
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
        (&Method::GET, "/.well-known/susi") => {
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
                #[serde(default)]
                encoding_format: Option<String>,
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
            let texts = match embedding_texts(&req.input) {
                Ok(texts) => texts,
                Err(message) => return Ok(api_error(StatusCode::BAD_REQUEST, message)),
            };
            let base64 = match req.encoding_format.as_deref() {
                None | Some("float") => false,
                Some("base64") => true,
                Some(_) => {
                    return Ok(api_error(
                        StatusCode::BAD_REQUEST,
                        "encoding_format must be 'float' or 'base64'",
                    ));
                }
            };
            let prompt_tokens = texts.iter().map(|t| approx_tokens(t)).sum::<u64>();
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
                    v.map(|e| {
                        let embedding = if base64 {
                            json!(embedding_base64(&e))
                        } else {
                            json!(e)
                        };
                        json!({"object": "embedding", "index": i, "embedding": embedding})
                    })
                })
                .collect();
            let payload = json!({
                "object": "list",
                "data": data,
                "model": req.model.unwrap_or_else(|| "susi-embed".to_string()),
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "total_tokens": prompt_tokens,
                    "estimated": true
                },
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
            let placement = if let Some(model) = completion.model.as_deref() {
                let (routable, cooled) = provider_ids();
                let is_cloud = routable
                    .iter()
                    .chain(cooled.iter())
                    .any(|provider| provider == model);
                if let Some(message) = explicit_model_conflict(
                    model,
                    is_cloud,
                    completion.requires.as_deref(),
                    completion.max_cost,
                    completion.allow_cloud,
                ) {
                    return Ok(api_error(StatusCode::BAD_REQUEST, message));
                }
                let mut decision = gemi::ModelManager::placement_with_cloud(
                    completion.requires.as_deref(),
                    completion.max_cost,
                    completion.allow_cloud,
                );
                if decision.get("contract").is_none() {
                    return Ok(api_error(
                        StatusCode::BAD_GATEWAY,
                        "placement plane returned no versioned contract",
                    ));
                }
                if let Some(object) = decision.as_object_mut() {
                    object.insert("target".to_string(), json!("explicit"));
                    object.insert("provider".to_string(), json!(model));
                    object.insert("reason".to_string(), json!("caller pinned model"));
                }
                decision
            } else {
                gemi::ModelManager::placement_with_cloud(
                    completion.requires.as_deref(),
                    completion.max_cost,
                    completion.allow_cloud,
                )
            };
            if placement.get("target").and_then(|value| value.as_str()) == Some("unavailable") {
                return Ok(api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    placement
                        .get("reason")
                        .and_then(|value| value.as_str())
                        .unwrap_or("no eligible inference target is available"),
                ));
            }
            let planned_model = placement
                .get("provider")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    (!completion.allow_cloud)
                        .then(|| {
                            placement
                                .get("local_model")
                                .and_then(|value| value.as_str())
                        })
                        .flatten()
                })
                .map(str::to_string);
            let placement_target = placement
                .get("target")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_string();
            let placement_id = placement
                .pointer("/contract/decision_id")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_string();
            let is_auto_alias = completion
                .model
                .as_deref()
                .map(|m| matches!(m.to_ascii_lowercase().as_str(), "default" | "susi" | "auto"))
                .unwrap_or(false);
            let requested_model = if is_auto_alias {
                planned_model
            } else {
                completion.model.clone().or(planned_model)
            };
            // OpenAI contract: an unknown `model` is a 400, not a silent
            // reroute. Known names are honored by the governed pipeline's
            // own selection (the response labels the model that served).
            if let Some(ref requested) = completion.model {
                let is_alias = matches!(
                    requested.to_ascii_lowercase().as_str(),
                    "default"
                        | "susi"
                        | "auto"
                        | "gpt-4"
                        | "gpt-4o"
                        | "gpt-4o-mini"
                        | "gpt-3.5-turbo"
                        | "claude-3-5-sonnet"
                        | "claude-3-7-sonnet"
                        | "deepseek-chat"
                        | "qwen2.5-coder:7b"
                );
                let known = is_alias
                    || gemi::ModelManager::list_models(&workspace)
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
                // Registered provider backends are also valid `model` ids —
                // the governed cascade honors them as routing hints, and
                // response labels echo them back verbatim.
                let known = known || {
                    let (order, cooled) = provider_ids();
                    order.iter().chain(cooled.iter()).any(|p| p == requested)
                };
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
            // Multi-turn requests are assembled cache-safely by the GEMI
            // plane — instructions hoisted, history verbatim — and the
            // measured cacheable fraction journaled against the provider
            // the placement just chose (VC-202-006).
            let pulse_intent = match completion.turns {
                Some(turns) => assemble_context_prompt(&turns, completion_provider(&placement)),
                None => completion.prompt,
            };
            let intent = gemi::IntentClassifier::classify(&pulse_intent);
            // A caller-requested model labels the response; intent
            // classification only applies when no model was named. The
            // label is the display id (stem), matching /v1/models.
            let resolved_model = requested_model.clone().unwrap_or_else(|| {
                gemi::ModelManager::get_active_engine_and_model(Some(&intent)).1
            });
            let active_model = model_display_id(&resolved_model).to_string();
            // The durable audit tier is payload-free: a prompt rides as a
            // digest plus byte length — provable, never readable (VC-202-018).
            crate::susi_sandbox::manager::SusiAuditLogger::log_details(
                &workspace,
                crate::susi_sandbox::manager::LogLevel::Info,
                "WEB_MISSION_START",
                &crate::susi_sandbox::audit_fidelity::AuditDetails::payload_ref(
                    &pulse_intent,
                    "prompt",
                ),
            );
            // A compact record that fits the audit detail cap, so the
            // decision id returned below resolves through the MCP
            // `placement_decision` tool.
            crate::susi_sandbox::manager::SusiAuditLogger::log_event(
                &workspace,
                "INFERENCE_PLACEMENT",
                &crate::susi_sandbox::placement_audit::placement_audit_details(&placement),
            );
            crate::susi_sandbox::manager::SusiAuditLogger::log_event(
                &workspace,
                "CHAT_COMPLETION_REQUEST",
                &format!(
                    "model: {active_model}, streaming: {is_streaming}, target: {placement_target}"
                ),
            );
            let trimmed_prompt = pulse_intent.trim().to_string();

            if is_streaming {
                Ok(build_streaming_response(
                    trimmed_prompt,
                    active_model,
                    requested_model.clone(),
                    completion.max_tokens,
                    completion.stop.clone(),
                    completion.include_usage,
                    Arc::clone(&workspace),
                    path == "/v1/completions",
                    permit,
                    &placement_target,
                    &placement_id,
                    completion.requires.clone(),
                ))
            } else {
                let ws = (*workspace).clone();
                let prompt_for_task = trimmed_prompt.clone();
                let model_for_task = requested_model;
                let requires_for_task = completion.requires.clone();
                let content = match tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let final_resp = gawd::solve_mission_generative(
                        &prompt_for_task,
                        &ws,
                        env!("CARGO_PKG_VERSION"),
                        model_for_task.as_deref(),
                        requires_for_task.as_deref(),
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

                if let Some(detail) = content.strip_prefix("[INFERENCE_FAILED]") {
                    // The detail arrives with or without a leading newline;
                    // without one it ran into the headline ("failedRecovery").
                    let detail = detail.trim();
                    let message = if detail.is_empty() {
                        "All inference providers failed".to_string()
                    } else {
                        format!("All inference providers failed: {detail}")
                    };
                    return Ok(json_response(
                        StatusCode::BAD_GATEWAY,
                        &json!({"error": {"message": message, "type": "server_error", "code": "inference_exhausted"}}),
                    ));
                }
                // Label the response with the actual generator, not the
                // requested model — failover may have served it. The
                // rendered inference receipt names it as
                // `Source: inference:<provider>`; "local" means the
                // resolved local model generated the answer.
                let served_model = content
                    .strip_prefix("Source: inference:")
                    .and_then(|rest| rest.split('|').next())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|gen_name| {
                        if gen_name == "local" {
                            active_model.as_str()
                        } else {
                            gen_name
                        }
                    })
                    .unwrap_or(&active_model)
                    .to_string();
                // The receipt wrapper is audit metadata — the response
                // body carries the raw model output, matching the
                // streaming path's shape.
                let body_text = unwrap_inference_render(&content).unwrap_or(content);
                let body_text = apply_stops(&body_text, &completion.stop);
                let (body_text, capped) = cap_completion(&body_text, completion.max_tokens);
                let mut payload = completion_response(
                    &served_model,
                    &body_text,
                    path == "/v1/completions",
                    if capped { "length" } else { "stop" },
                );
                payload["usage"] =
                    estimated_usage(approx_tokens(&trimmed_prompt), approx_tokens(&body_text));
                add_placement_body(&mut payload, &placement_id, &placement_target);
                let mut response = json_response(StatusCode::OK, &payload);
                add_placement_headers(&mut response, &placement_target, &placement_id);
                Ok(response)
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
                    HeaderValue::from_static(
                        "Authorization, Content-Type, X-Api-Key, Anthropic-Version, X-Susi-Node, X-Susi-Req-Ts, X-Susi-Req-Nonce, X-Susi-Req-Sig",
                    ),
                )
                .body(full_body(Vec::new()))
                .unwrap_or_else(|_| {
                    api_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to build response")
                }))
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
                let events = graph.workspace_events(&ws);
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
        (&Method::GET, "/metrics") => {
            let available_slots = admission.available_permits();
            let samples = tokio::task::spawn_blocking(move || {
                metric_samples(
                    available_slots,
                    &gemi::loaded_models(),
                    &gemi::sample_telemetry(),
                )
            })
            .await
            .unwrap_or_else(|_| metric_samples(available_slots, &json!({}), &json!({})));
            Ok(body_response(
                StatusCode::OK,
                PROMETHEUS_CONTENT_TYPE,
                prometheus_metrics::render_prometheus(&samples),
            ))
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
        _ => Ok(api_error(StatusCode::NOT_FOUND, "Endpoint not found")),
    }
}

#[allow(clippy::too_many_arguments)] // response constructor: the two
// model strings serve different roles (response label vs routing hint)
// and the permit must travel with the request — a struct adds a type
// for one call site.
fn build_streaming_response(
    prompt: String,
    model_name: String,
    requested_model: Option<String>,
    max_tokens: Option<u32>,
    stop: Vec<String>,
    include_usage: bool,
    workspace: Arc<PathBuf>,
    legacy: bool,
    permit: tokio::sync::OwnedSemaphorePermit,
    placement_target: &str,
    placement_id: &str,
    requires: Option<String>,
) -> Response<BoxBody> {
    let prompt_tokens = approx_tokens(&prompt);
    let rx = completion_stream(
        model_name,
        legacy,
        max_tokens,
        stop,
        include_usage,
        prompt_tokens,
        Some(placement_value(placement_id, placement_target)),
        move |callback, meta| {
            let _permit = permit;
            // A `/v1` completion request: a local engine it needs may be
            // started on demand (core starts none on its own).
            let _user_request = susi_core::provider::OnDemandEngineStart::allow();
            gemi::GemiEngine::generate_reasoning_stream_with_model_meta(
                &prompt,
                &workspace,
                &|chunk| {
                    callback(chunk);
                },
                requested_model.as_deref(),
                meta,
                requires.as_deref(),
            )
        },
    );
    let body = MpscBody(rx).boxed();

    let mut response = Response::builder()
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
        .unwrap_or_else(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to build response",
            )
        });
    add_placement_headers(&mut response, placement_target, placement_id);
    response
}

fn add_placement_headers(response: &mut Response<BoxBody>, target: &str, decision_id: &str) {
    let target = HeaderValue::from_str(target);
    let decision_id = HeaderValue::from_str(decision_id);
    if let (Ok(target), Ok(decision_id)) = (target, decision_id) {
        response.headers_mut().insert("X-Susi-Placement", target);
        response
            .headers_mut()
            .insert("X-Susi-Placement-Id", decision_id);
        response.headers_mut().insert(
            "Access-Control-Expose-Headers",
            HeaderValue::from_static("X-Susi-Placement, X-Susi-Placement-Id"),
        );
    }
}

/// Placement correlation carried in response bodies, for SDKs and proxies
/// that discard the `X-Susi-Placement*` headers.
fn placement_value(decision_id: &str, target: &str) -> serde_json::Value {
    json!({
        "decision_id": decision_id,
        "target": target,
    })
}

fn add_placement_body(payload: &mut serde_json::Value, decision_id: &str, target: &str) {
    payload["susi_placement"] = placement_value(decision_id, target);
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
/// this so a client can echo `id` back into `model` verbatim. Both path
/// separators count, so a Windows path (`C:\...\m.gguf`) never leaks the
/// user's directory layout to API clients.
fn model_display_id(id: &str) -> &str {
    if !id.contains(['/', '\\']) {
        return id;
    }
    let name = id.rsplit(['/', '\\']).next().unwrap_or(id);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// Registered cloud/provider backends as selectable `model` ids.
/// `cloud_failover_order` returns the currently routable order plus the
/// cooled set — validation accepts both (a cooled provider is known, just
/// resting), while `/v1/models` advertises only the routable ones.
fn provider_ids() -> (Vec<String>, Vec<String>) {
    let v = gemi::cloud_failover_order();
    let list = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    (list("order"), list("cooled"))
}

/// `/v1/models` entries in OpenAI shape. `id` is the display id a client
/// echoes back as `model`; the raw internal id stays as `model_id`.
fn openai_model_list(workspace: &std::path::Path) -> Vec<serde_json::Value> {
    let models = gemi::ModelManager::list_models(workspace);
    let mut out: Vec<serde_json::Value> = models
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
        .unwrap_or_default();
    // Provider ids are advertised verbatim — `model_display_id` would
    // mangle `vendor/model` names through Path::file_stem.
    for name in provider_ids().0 {
        out.push(json!({
            "id": name,
            "model_id": name,
            "object": "model",
            "owned_by": "susi",
        }));
    }
    for alias in ["susi", "auto", "gpt-4o"] {
        out.push(json!({
            "id": alias,
            "model_id": alias,
            "object": "model",
            "owned_by": "susi",
        }));
    }
    out
}

/// Generative answers render through an `inference:<provider>` receipt —
/// the ledger keeps the provenance; the OpenAI-compat response body must
/// carry the raw model output (the streaming path already returns it
/// unwrapped). Returns `None` when `content` is not exactly one rendered
/// inference receipt.
fn unwrap_inference_render(content: &str) -> Option<String> {
    let rest = content.strip_prefix("Source: inference:")?;
    let body = rest.split_once("\nTool reported:\n")?.1;
    let mut lines = Vec::new();
    for line in body.lines() {
        lines.push(line.strip_prefix("> ").unwrap_or(line));
    }
    Some(lines.join("\n"))
}

/// Cut `text` at `max_words` words or `max_chars` bytes, whichever binds
/// first, at a UTF-8 boundary. Returns bytes kept and whether it cut.
fn cap_piece(text: &str, max_words: usize, max_chars: usize) -> (usize, bool) {
    let mut end = text.len().min(max_chars);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut in_word = false;
    let mut words = 0usize;
    for (i, ch) in text[..end].char_indices() {
        if ch.is_whitespace() {
            in_word = false;
        } else if !in_word {
            in_word = true;
            words += 1;
            if words > max_words {
                end = i;
                break;
            }
        }
    }
    (end, end < text.len())
}

/// Approximate-token cap for `max_tokens`. Without the provider's own
/// tokenizer an exact count is impossible, so enforce whichever bound
/// binds first — `max` whitespace-separated words (a word is never less
/// than one token) or `max * 4` chars (~4 chars/token for dense text).
/// Returns the served text and whether it was cut, so callers can report
/// `finish_reason: "length"` honestly.
fn cap_completion(text: &str, max_tokens: Option<u32>) -> (String, bool) {
    let Some(max) = max_tokens.map(|m| m as usize).filter(|m| *m > 0) else {
        return (text.to_string(), false);
    };
    let (end, cut) = cap_piece(text, max, max.saturating_mul(4));
    if cut {
        (text[..end].trim_end().to_string(), true)
    } else {
        (text.to_string(), false)
    }
}

/// Byte position of the earliest occurrence of any `stop` sequence in
/// `text`, or `None`. Content before it is served; the stop sequence
/// itself is never included in the output (OpenAI semantics).
fn first_stop(text: &str, stops: &[String]) -> Option<usize> {
    stops.iter().filter_map(|s| text.find(s.as_str())).min()
}

/// Truncate `text` at the earliest stop-sequence occurrence.
fn apply_stops(text: &str, stops: &[String]) -> String {
    match first_stop(text, stops) {
        Some(pos) => text[..pos].to_string(),
        None => text.to_string(),
    }
}

/// Length of the longest suffix of `text` that is a proper prefix of any
/// stop sequence. That many bytes must be held back before emitting so a
/// stop sequence spanning a chunk boundary is still detected.
fn partial_stop_hold(text: &str, stops: &[String]) -> usize {
    let bytes = text.as_bytes();
    let mut hold = 0;
    for s in stops {
        let sb = s.as_bytes();
        for n in 1..sb.len().min(bytes.len() + 1) {
            if n > hold && bytes.ends_with(&sb[..n]) {
                hold = n;
            }
        }
    }
    hold
}

fn completion_response(
    model: &str,
    content: &str,
    legacy: bool,
    finish_reason: &str,
) -> serde_json::Value {
    let choice = if legacy {
        json!({"index": 0, "text": content, "finish_reason": finish_reason, "logprobs": null})
    } else {
        json!({"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": finish_reason})
    };
    json!({"id": completion_id(), "object": if legacy { "text_completion" } else { "chat.completion" },
        "created": now_secs(), "model": model, "choices": [choice]})
}

fn stream_chunk(
    identity: (&str, u64),
    model: &str,
    legacy: bool,
    delta: serde_json::Value,
    finish_reason: Option<&str>,
) -> String {
    format!(
        "data: {}\n\n",
        stream_chunk_value(identity, model, legacy, delta, finish_reason)
    )
}

fn stream_chunk_value(
    identity: (&str, u64),
    model: &str,
    legacy: bool,
    delta: serde_json::Value,
    finish_reason: Option<&str>,
) -> serde_json::Value {
    let (id, created) = identity;
    let reason = finish_reason
        .map(|r| json!(r))
        .unwrap_or(serde_json::Value::Null);
    let choice = if legacy {
        json!({"index": 0, "text": delta.get("content").and_then(|v| v.as_str()).unwrap_or(""),
            "finish_reason": reason, "logprobs": null})
    } else {
        json!({"index": 0, "delta": delta, "finish_reason": reason})
    };
    json!({"id": id, "created": created, "model": model,
    "object": if legacy { "text_completion" } else { "chat.completion.chunk" }, "choices": [choice]})
}

#[allow(clippy::too_many_arguments)] // response-shape params travel
// together; a struct for one call site adds a type without clarity.
fn completion_stream(
    model: String,
    legacy: bool,
    max_tokens: Option<u32>,
    stop: Vec<String>,
    include_usage: bool,
    prompt_tokens: u64,
    placement: Option<serde_json::Value>,
    solve: impl FnOnce(&dyn Fn(String), &dyn Fn(&str)) -> String + Send + 'static,
) -> tokio::sync::mpsc::Receiver<String> {
    // Bound queued frames and split large frames so a slow client cannot
    // retain an unbounded response queue. Blocking sends run only on the blocking pool.
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let error_tx = tx.clone();
    let task = tokio::task::spawn_blocking(move || {
        let id = completion_id();
        let created = now_secs();
        // The solver reports the actual serving backend through the meta
        // callback before content starts; until then fall back to the
        // requested/resolved label so early paths (reflex solves) stay sane.
        let actual: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
        let role_sent = std::cell::Cell::new(false);
        let label = || actual.borrow().clone().unwrap_or_else(|| model.clone());
        // The role frame is deferred until the meta report or the first
        // content chunk so its `model` field names the true generator.
        let send_role = |tx: &tokio::sync::mpsc::Sender<String>| -> bool {
            if role_sent.replace(true) {
                return true;
            }
            // Placement correlation rides the opening frame only: it is a
            // per-response fact, not per-chunk content.
            let mut frame = stream_chunk_value(
                (&id, created),
                &label(),
                legacy,
                json!({"role": "assistant"}),
                None,
            );
            if let Some(placement) = &placement {
                frame["susi_placement"] = placement.clone();
            }
            tx.blocking_send(format!("data: {frame}\n\n")).is_ok()
        };
        let meta_cb = |name: &str| {
            *actual.borrow_mut() = Some(name.to_string());
            let _ = send_role(&tx);
        };
        let emitted = std::cell::Cell::new(false);
        let emitted_words = std::cell::Cell::new(0usize);
        let emitted_chars = std::cell::Cell::new(0usize);
        // Total emitted text for `usage` reporting — independent of the
        // max_tokens budget counters above, which only advance when a
        // budget exists.
        let emitted_total_chars = std::cell::Cell::new(0u64);
        let emitted_total_words = std::cell::Cell::new(0u64);
        let truncated = std::cell::Cell::new(false);
        let failed = std::cell::Cell::new(false);
        // `stop` sequences may span chunk boundaries — content is buffered
        // and only the portion that cannot yet complete a match is emitted.
        let pending = std::cell::RefCell::new(String::new());
        let stopped = std::cell::Cell::new(false);
        // Sends screened content through the token budget and chunk loop.
        let send_content = |mut piece: String| {
            if piece.is_empty() || truncated.get() {
                return;
            }
            if !send_role(&tx) {
                return;
            }
            emitted.set(true);
            // `max_tokens` budget: cut this piece at the remaining word/char
            // allowance, then suppress the rest of the stream.
            if let Some(max) = max_tokens.map(|m| m as usize) {
                let (keep, cut) = cap_piece(
                    &piece,
                    max.saturating_sub(emitted_words.get()),
                    max.saturating_mul(4).saturating_sub(emitted_chars.get()),
                );
                emitted_words.set(emitted_words.get() + piece[..keep].split_whitespace().count());
                emitted_chars.set(emitted_chars.get() + keep);
                if cut {
                    truncated.set(true);
                }
                piece.truncate(keep);
                if piece.is_empty() {
                    return;
                }
            }
            emitted_total_chars.set(emitted_total_chars.get() + piece.len() as u64);
            emitted_total_words
                .set(emitted_total_words.get() + piece.split_whitespace().count() as u64);
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
                        &label(),
                        legacy,
                        json!({"content": chunk}),
                        None,
                    ))
                    .is_err()
                {
                    return;
                }
                remaining = rest;
            }
        };
        let callback = |piece: String| {
            if piece.is_empty() || truncated.get() || stopped.get() {
                return;
            }
            // Engine failure markers are not content — suppress them and
            // terminate the stream with an SSE error frame instead.
            let head = piece.trim_start();
            if head.starts_with("[FAIL]") || head.starts_with("[INFERENCE_FAILED]") {
                failed.set(true);
                return;
            }
            if stop.is_empty() {
                send_content(piece);
                return;
            }
            pending.borrow_mut().push_str(&piece);
            let mut buf = pending.take();
            if let Some(pos) = first_stop(&buf, &stop) {
                buf.truncate(pos);
                stopped.set(true);
            } else {
                let hold = partial_stop_hold(&buf, &stop);
                let keep = buf.len() - hold;
                let tail = buf.split_off(keep);
                *pending.borrow_mut() = tail;
            }
            send_content(buf);
        };
        let result = solve(&callback, &meta_cb);
        // Some solver paths return a complete answer without invoking callbacks.
        if !emitted.get() && !failed.get() && !stopped.get() {
            callback(result);
        }
        // Flush the held-back tail — it was verified stop-free each round.
        if !stopped.get() && !failed.get() {
            let rest = pending.take();
            if !rest.is_empty() {
                send_content(rest);
            }
        }
        if failed.get() {
            let _ = tx.blocking_send(format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"error": {
                    "message": "inference failed", "type": "server_error",
                    "code": "inference_exhausted"
                }})
            ));
            return;
        }
        // A solver that produced neither meta nor content still owes the
        // client the opening role frame before the finish frame.
        if !send_role(&tx) {
            return;
        }
        if tx
            .blocking_send(stream_chunk(
                (&id, created),
                &label(),
                legacy,
                json!({}),
                Some(if truncated.get() { "length" } else { "stop" }),
            ))
            .is_err()
        {
            return;
        }
        // `stream_options.include_usage`: a terminal choices:[] chunk
        // carrying the token accounting between the finish chunk and
        // [DONE], per the OpenAI streaming contract.
        if include_usage {
            let completion_tokens = (emitted_total_chars.get() / 4).max(emitted_total_words.get());
            let _ = tx.blocking_send(format!(
                "data: {}\n\n",
                json!({"id": id, "created": created, "model": label(),
                "object": if legacy { "text_completion" } else { "chat.completion.chunk" },
                "choices": [],
                "usage": estimated_usage(prompt_tokens, completion_tokens)})
            ));
        }
        let _ = tx.blocking_send("data: [DONE]\n\n".to_owned());
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

/// Prometheus text exposition format 0.0.4.
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The gauges `GET /metrics` exposes, read from live state: this surface's
/// free completion slots, the GEMI plane's inference cache, and the host
/// telemetry snapshot. A reading the host cannot provide is omitted, never
/// reported as zero.
fn metric_samples(
    available_slots: usize,
    loaded_models: &serde_json::Value,
    telemetry: &serde_json::Value,
) -> Vec<prometheus_metrics::MetricSample> {
    fn gauge(name: &str, value: f64, help: &str) -> prometheus_metrics::MetricSample {
        prometheus_metrics::MetricSample {
            name: name.to_string(),
            value,
            help: help.to_string(),
        }
    }
    const MIB: f64 = 1024.0 * 1024.0;
    let mut samples = vec![
        gauge("susi_up", 1.0, "The GEMI HTTP surface is serving requests."),
        gauge(
            "susi_gemi_completion_slots_available",
            available_slots as f64,
            "Completion admission slots free right now.",
        ),
    ];
    if let Some(models) = loaded_models.get("models").and_then(|m| m.as_array()) {
        samples.push(gauge(
            "susi_inference_models_loaded",
            models.len() as f64,
            "Models resident in the GEMI inference cache.",
        ));
    }
    let reading = |key: &str| telemetry.get(key).and_then(serde_json::Value::as_f64);
    if let Some(value) = reading("cpu_util_pct") {
        samples.push(gauge(
            "susi_host_cpu_utilization_percent",
            value,
            "Host-wide CPU utilization since the previous sample.",
        ));
    }
    if let Some(value) = reading("load_avg_1m") {
        samples.push(gauge(
            "susi_host_load_average_1m",
            value,
            "Host one-minute load average.",
        ));
    }
    if let Some(value) = reading("mem_total_mb") {
        samples.push(gauge(
            "susi_host_memory_total_bytes",
            value * MIB,
            "Installed physical memory.",
        ));
    }
    if let Some(value) = reading("mem_available_mb") {
        samples.push(gauge(
            "susi_host_memory_available_bytes",
            value * MIB,
            "Memory available to new work.",
        ));
    }
    samples
}

/// `POST /v1/messages`: an Anthropic Messages request, translated to a chat
/// completion, served by the same handler as `POST /v1/chat/completions`
/// (placement, admission, audit), and translated back. `stream: true` gets
/// the Anthropic SSE event sequence for the assembled answer.
async fn anthropic_messages(
    body: &[u8],
    workspace: Arc<PathBuf>,
    admission: Arc<tokio::sync::Semaphore>,
) -> Response<BoxBody> {
    let request: serde_json::Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(error) => {
            return anthropic_error(StatusCode::BAD_REQUEST, &format!("invalid JSON: {error}"));
        }
    };
    let mut chat = match anthropic_gateway::to_chat_request(&request) {
        Ok(chat) => chat,
        Err(message) => return anthropic_error(StatusCode::BAD_REQUEST, &message),
    };
    let requested_model = chat["model"].as_str().unwrap_or_default().to_string();
    let stream = chat["stream"].as_bool().unwrap_or(false);
    // The SSE sequence is derived from the assembled answer, so the inner
    // completion always runs unstreamed.
    chat["stream"] = json!(false);
    let chat_body = Bytes::from(serde_json::to_vec(&chat).unwrap_or_default());
    let inner = match dispatch_gemi_route(
        Method::POST,
        "/v1/chat/completions".to_string(),
        chat_body,
        workspace,
        admission,
    )
    .await
    {
        Ok(response) => response,
        Err(never) => match never {},
    };
    let (parts, inner_body) = inner.into_parts();
    let bytes = match inner_body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(never) => match never {},
    };
    let completion: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    let mut response = anthropic_response(parts.status, &completion, &requested_model, stream);
    for name in [
        "X-Susi-Placement",
        "X-Susi-Placement-Id",
        "Access-Control-Expose-Headers",
        "Retry-After",
    ] {
        if let Some(value) = parts.headers.get(name) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

/// The Anthropic response for a finished chat completion: the Messages
/// document, its SSE event sequence when `stream`, or the Anthropic error
/// envelope carrying the completion's status and message.
fn anthropic_response(
    status: StatusCode,
    completion: &serde_json::Value,
    requested_model: &str,
    stream: bool,
) -> Response<BoxBody> {
    if !status.is_success() {
        return anthropic_error(
            status,
            completion
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or("inference request failed"),
        );
    }
    let model = completion
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(requested_model);
    if stream {
        let events: String = anthropic_gateway::stream_events(completion, model)
            .into_iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect();
        body_response(StatusCode::OK, "text/event-stream", events)
    } else {
        json_response(
            StatusCode::OK,
            &anthropic_gateway::from_chat_response(completion, model),
        )
    }
}

/// Anthropic error envelope (`{"type":"error","error":{type,message}}`) with
/// the error type Anthropic SDKs map from each status.
fn anthropic_error(status: StatusCode, message: &str) -> Response<BoxBody> {
    let kind = match status {
        StatusCode::BAD_REQUEST => "invalid_request_error",
        StatusCode::UNAUTHORIZED => "authentication_error",
        StatusCode::FORBIDDEN => "permission_error",
        StatusCode::NOT_FOUND => "not_found_error",
        StatusCode::PAYLOAD_TOO_LARGE => "request_too_large",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        _ => "api_error",
    };
    let mut response = json_response(
        status,
        &json!({"type": "error", "error": {"type": kind, "message": message}}),
    );
    // The same RFC 9110 challenge and RFC 6585 retry hint `api_error` sends.
    let headers = response.headers_mut();
    if status == StatusCode::UNAUTHORIZED {
        headers.insert(
            hyper::header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"susi-gemi\""),
        );
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        headers.insert(hyper::header::RETRY_AFTER, HeaderValue::from_static("60"));
    }
    response
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// OpenAI-shaped error envelope (`error.{message,type,param,code}`) whose
/// `type`/`code` follow the status the way the OpenAI API reports them, so
/// SDK error classes map correctly. A 401 carries its `WWW-Authenticate`
/// challenge (RFC 9110 §15.5.2) and a 429 its `Retry-After` (RFC 6585 §4).
fn api_error(status: StatusCode, message: &str) -> Response<BoxBody> {
    let (kind, code) = match status {
        StatusCode::UNAUTHORIZED => ("invalid_request_error", json!("invalid_api_key")),
        StatusCode::NOT_FOUND => ("invalid_request_error", json!("not_found")),
        StatusCode::TOO_MANY_REQUESTS => ("requests", json!("rate_limit_exceeded")),
        s if s.is_server_error() => ("server_error", serde_json::Value::Null),
        _ => ("invalid_request_error", serde_json::Value::Null),
    };
    let mut response = json_response(
        status,
        &json!({"error": {
            "message": message, "type": kind, "param": null, "code": code
        }}),
    );
    let headers = response.headers_mut();
    if status == StatusCode::UNAUTHORIZED {
        headers.insert(
            hyper::header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"susi-gemi\""),
        );
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        headers.insert(hyper::header::RETRY_AFTER, HeaderValue::from_static("60"));
    }
    response
}

struct CompletionInput {
    prompt: String,
    /// Raw `(role, content)` turns for multi-message requests — the prompt
    /// is assembled cache-safely by the GEMI plane once the placement has
    /// chosen the provider that prices the measured saving.
    turns: Option<Vec<(String, String)>>,
    stream: bool,
    model: Option<String>,
    max_tokens: Option<u32>,
    stop: Vec<String>,
    /// `stream_options.include_usage` — OpenAI emits a final
    /// `choices:[]` usage chunk before [DONE] only when requested.
    include_usage: bool,
    requires: Option<String>,
    max_cost: Option<f64>,
    allow_cloud: bool,
}

/// The provider a placement decision routes to — priced for the measured
/// cache saving; `None` for local targets, which carry no catalog price.
fn completion_provider(placement: &serde_json::Value) -> Option<&str> {
    placement.get("provider").and_then(|v| v.as_str())
}

/// Cache-first context assembly and its measurement journal live in the
/// GEMI plane (`gemi.context.assemble`): the server sends the raw turns and
/// gets back a cache-safe prompt. A plane that is not wired — a partial
/// test harness — falls back to the literal-order join rather than failing
/// the request, with no journal entry (there is nothing measured).
fn assemble_context_prompt(turns: &[(String, String)], provider: Option<&str>) -> String {
    let payload = serde_json::json!({
        "turns": turns
            .iter()
            .map(|(role, content)| serde_json::json!({"role": role, "content": content}))
            .collect::<Vec<_>>(),
        "provider": provider,
    });
    susi_core::plane_bus::PlaneBus::global()
        .request("gemi.context.assemble", payload)
        .ok()
        .and_then(|v| v.get("prompt").and_then(|p| p.as_str()).map(str::to_string))
        .unwrap_or_else(|| {
            turns
                .iter()
                .map(|(role, content)| format!("{role}: {content}"))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
}

fn parse_completion(body: &[u8], legacy: bool) -> Result<CompletionInput, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {e}"))?;
    let object = value.as_object().ok_or("Request must be a JSON object")?;
    let stream = match object.get("stream") {
        None => false,
        Some(value) => value.as_bool().ok_or("stream must be a boolean")?,
    };
    let (prompt, turns) = if legacy {
        (
            object
                .get("prompt")
                .and_then(|p| p.as_str())
                .ok_or("prompt must be a non-empty string")?
                .to_owned(),
            None,
        )
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
                .filter(|r| {
                    matches!(
                        *r,
                        "system" | "developer" | "user" | "assistant" | "tool" | "function"
                    )
                })
                .ok_or(
                    "Each message must have a supported role: system, developer, user, assistant, tool, function",
                )?;
            let content = match message.get("content") {
                Some(serde_json::Value::String(text)) => text.clone(),
                Some(serde_json::Value::Array(parts)) if !parts.is_empty() => {
                    let mut text = String::new();
                    for part in parts {
                        if part.get("type").and_then(|t| t.as_str()) == Some("text")
                            && let Some(t) = part.get("text").and_then(|t| t.as_str())
                        {
                            text.push_str(t);
                        }
                    }
                    text
                }
                Some(serde_json::Value::Null) | None => String::new(),
                _ => return Err("Each message requires string or text-part content".into()),
            };

            // Support assistant messages with tool_calls, or tool/function result messages
            let content = if content.trim().is_empty() {
                if let Some(tool_calls) = message.get("tool_calls").and_then(|tc| tc.as_array()) {
                    let mut tc_desc = Vec::new();
                    for tc in tool_calls {
                        let name = tc
                            .pointer("/function/name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("function");
                        let args = tc
                            .pointer("/function/arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("{}");
                        tc_desc.push(format!("{name}({args})"));
                    }
                    format!("tool_calls: [{}]", tc_desc.join(", "))
                } else if matches!(role, "assistant" | "tool" | "function") {
                    String::new()
                } else {
                    return Err("Message content must not be empty".into());
                }
            } else {
                content
            };

            let formatted_turn = if role == "tool" || role == "function" {
                if let Some(tool_call_id) = message.get("tool_call_id").and_then(|id| id.as_str()) {
                    format!("[tool_call_id: {tool_call_id}] {content}")
                } else {
                    content
                }
            } else {
                content
            };

            turns.push((role.to_string(), formatted_turn));
        }
        // Preserve the direct single-user task path; multi-turn requests
        // carry their roles for the GEMI plane's cache-first assembly.
        if turns.len() == 1 && turns[0].0 == "user" {
            (turns.remove(0).1, None)
        } else {
            (String::new(), Some(turns))
        }
    };
    if turns.is_none() && prompt.trim().is_empty() {
        return Err("Prompt must not be empty".into());
    }
    let model = object
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string);
    // `max_tokens` (legacy) and `max_completion_tokens` (current) alias the
    // same budget; a present-but-invalid value is a client error, not
    // silence.
    let max_tokens = match object
        .get("max_tokens")
        .or_else(|| object.get("max_completion_tokens"))
    {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| *n > 0 && *n <= u64::from(u32::MAX))
                .ok_or("max_tokens must be a positive integer")? as u32,
        ),
    };
    // `stop`: a string or an array of strings; empty entries are no-ops.
    let stop = match object.get("stop") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(items)) => {
            let mut seqs = Vec::with_capacity(items.len());
            for item in items {
                seqs.push(
                    item.as_str()
                        .ok_or("stop entries must be strings")?
                        .to_string(),
                );
            }
            seqs
        }
        Some(_) => return Err("stop must be a string or an array of strings".into()),
    };
    let stop: Vec<String> = stop.into_iter().filter(|s| !s.is_empty()).collect();
    // Only single-choice completions exist — reject multi-n rather than
    // silently returning fewer choices than the client asked for.
    if let Some(n) = object.get("n")
        && n.as_u64() != Some(1)
    {
        return Err("only n=1 completions are supported".into());
    }
    let include_usage = object
        .get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(|u| u.as_bool())
        .unwrap_or(false);
    // The same validation the MCP `inference_placement` tool applies.
    let gemi::PlacementConstraints {
        requires,
        max_cost,
        allow_cloud,
    } = gemi::placement_constraints(
        object.get("susi").and_then(|value| value.as_object()),
        "susi.",
    )?;
    Ok(CompletionInput {
        prompt,
        turns,
        stream,
        model,
        max_tokens,
        stop,
        include_usage,
        requires,
        max_cost,
        allow_cloud,
    })
}

/// Texts of an embeddings `input`: a non-empty string or a non-empty array
/// of non-empty strings. Pre-tokenized input (integer arrays) needs the
/// provider's tokenizer, which the bus does not expose — it is refused
/// rather than silently dropped.
fn embedding_texts(input: &serde_json::Value) -> Result<Vec<String>, &'static str> {
    const SHAPE: &str = "input must be a non-empty string or array of non-empty strings";
    let texts = match input {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(items) => {
            let mut texts = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_json::Value::String(s) => texts.push(s.clone()),
                    serde_json::Value::Number(_) | serde_json::Value::Array(_) => {
                        return Err("token-array input is not supported; send text");
                    }
                    serde_json::Value::Null
                    | serde_json::Value::Bool(_)
                    | serde_json::Value::Object(_) => return Err(SHAPE),
                }
            }
            texts
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Object(_) => return Err(SHAPE),
    };
    if texts.is_empty() || texts.iter().any(String::is_empty) {
        return Err(SHAPE);
    }
    Ok(texts)
}

/// `encoding_format: "base64"` — the vector's little-endian f32 bytes,
/// standard base64, as the OpenAI API returns it.
fn embedding_base64(embedding: &[f32]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = embedding.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(TABLE[((n >> 18) & 0x3f) as usize]));
        out.push(char::from(TABLE[((n >> 12) & 0x3f) as usize]));
        out.push(if chunk.len() > 1 {
            char::from(TABLE[((n >> 6) & 0x3f) as usize])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(TABLE[(n & 0x3f) as usize])
        } else {
            '='
        });
    }
    out
}

/// Token-count approximation for `usage` reporting — the true BPE count
/// lives inside the engine and is not surfaced through the bus, so this
/// uses the same words/chars≈4 estimate `cap_completion` enforces with.
fn approx_tokens(text: &str) -> u64 {
    (text.split_whitespace().count() as u64).max(text.len() as u64 / 4)
}

/// OpenAI-shaped `usage` from [`approx_tokens`] counts, flagged
/// `estimated` so clients that bill or budget on it (and SUSI's own cost
/// and budget ledgers) never mistake the estimate for tokenizer counts.
fn estimated_usage(prompt_tokens: u64, completion_tokens: u64) -> serde_json::Value {
    json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
        "estimated": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_never_expose_local_paths() {
        for (internal, advertised) in [
            (
                r"C:\Users\a\models\qwen2.5-0.5b-instruct-q4_0.gguf",
                "qwen2.5-0.5b-instruct-q4_0",
            ),
            ("/home/a/.susi/models/phi.gguf", "phi"),
            (r"\\server\share\m.gguf", "m"),
            ("catalog-deepseek-chat", "catalog-deepseek-chat"),
            ("model", "model"),
            ("/home/a/.hidden", ".hidden"),
        ] {
            assert_eq!(model_display_id(internal), advertised, "{internal}");
        }
    }

    #[test]
    fn metrics_expose_live_readings_and_omit_unavailable_ones() {
        let samples = metric_samples(
            3,
            &json!({"models": [{"path": "a.gguf"}, {"path": "b.gguf"}]}),
            &json!({"cpu_util_pct": 12.5, "mem_total_mb": 2, "load_avg_1m": null}),
        );
        let value = |name: &str| samples.iter().find(|s| s.name == name).map(|s| s.value);
        assert_eq!(value("susi_up"), Some(1.0));
        assert_eq!(value("susi_gemi_completion_slots_available"), Some(3.0));
        assert_eq!(value("susi_inference_models_loaded"), Some(2.0));
        assert_eq!(value("susi_host_cpu_utilization_percent"), Some(12.5));
        assert_eq!(
            value("susi_host_memory_total_bytes"),
            Some(2.0 * 1024.0 * 1024.0)
        );
        assert_eq!(value("susi_host_load_average_1m"), None);
        assert_eq!(value("susi_host_memory_available_bytes"), None);
        let bare = metric_samples(0, &json!({"error": "no plane"}), &json!({}));
        assert_eq!(
            bare.len(),
            2,
            "unavailable readings are omitted, not zeroed"
        );
        let body = prometheus_metrics::render_prometheus(&bare);
        assert!(body.contains("# TYPE susi_up gauge\nsusi_up 1\n"));
    }

    #[tokio::test]
    async fn anthropic_responses_translate_success_stream_and_error() {
        let completion = json!({
            "id": "chatcmpl-1",
            "model": "served-model",
            "choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1}
        });
        let read = |response: Response<BoxBody>| async move {
            let status = response.status();
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let bytes = match response.into_body().collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(never) => match never {},
            };
            (
                status,
                content_type,
                String::from_utf8(bytes.to_vec()).unwrap(),
            )
        };

        let (status, content_type, body) = read(anthropic_response(
            StatusCode::OK,
            &completion,
            "claude-3-5-sonnet",
            false,
        ))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "application/json");
        let message: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(message["type"], "message");
        assert_eq!(message["model"], "served-model");
        assert_eq!(message["content"][0]["text"], "hi");
        assert_eq!(message["stop_reason"], "end_turn");
        assert_eq!(message["usage"]["input_tokens"], 3);

        let (_, content_type, body) = read(anthropic_response(
            StatusCode::OK,
            &completion,
            "claude-3-5-sonnet",
            true,
        ))
        .await;
        assert_eq!(content_type, "text/event-stream");
        assert!(body.starts_with("event: message_start\ndata: "));
        assert!(body.contains("\"text\":\"hi\""));
        assert!(body.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));

        let failed =
            json!({"error": {"message": "unknown model 'x'", "type": "invalid_request_error"}});
        let (status, _, body) = read(anthropic_response(
            StatusCode::BAD_REQUEST,
            &failed,
            "x",
            false,
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let error: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(error["type"], "error");
        assert_eq!(error["error"]["type"], "invalid_request_error");
        assert_eq!(error["error"]["message"], "unknown model 'x'");
        let (status, _, body) = read(anthropic_error(StatusCode::TOO_MANY_REQUESTS, "busy")).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(body.contains("rate_limit_error"));
    }

    #[test]
    fn placement_header_exposes_safe_target() {
        let mut response = json_response(StatusCode::OK, &json!({}));
        add_placement_headers(&mut response, "cloud", "placement-test-1");
        assert_eq!(
            response
                .headers()
                .get("X-Susi-Placement")
                .and_then(|value| value.to_str().ok()),
            Some("cloud")
        );
        assert_eq!(
            response
                .headers()
                .get("X-Susi-Placement-Id")
                .and_then(|value| value.to_str().ok()),
            Some("placement-test-1")
        );
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Expose-Headers")
                .and_then(|value| value.to_str().ok()),
            Some("X-Susi-Placement, X-Susi-Placement-Id")
        );
    }

    #[test]
    fn placement_body_preserves_decision_correlation() {
        let mut payload = completion_response("model", "answer", false, "stop");
        add_placement_body(&mut payload, "placement-test-2", "local");
        assert_eq!(
            payload["susi_placement"],
            json!({"decision_id": "placement-test-2", "target": "local"})
        );
    }

    #[test]
    fn explicit_models_cannot_bypass_placement_constraints() {
        assert!(
            explicit_model_conflict("openai-gpt", true, None, None, false)
                .is_some_and(|message| message.contains("allow_cloud"))
        );
        assert!(
            explicit_model_conflict("local", false, Some("vision"), None, true)
                .is_some_and(|message| message.contains("requires=vision"))
        );
        assert_eq!(
            explicit_model_conflict("local", false, Some("code"), Some(0.0), false),
            None
        );
        assert_eq!(
            explicit_model_conflict("openrouter-free", true, Some("vision"), Some(0.0), true),
            None
        );
        assert!(
            explicit_model_conflict("openai-gpt", true, None, Some(0.0), true)
                .is_some_and(|message| message.contains("explicitly free"))
        );
    }

    #[test]
    fn completion_parses_susi_placement_constraints() {
        let input = parse_completion(
            br#"{"messages":[{"role":"user","content":"describe image"}],"susi":{"requires":"vision","max_cost":0.01,"allow_cloud":false}}"#,
            false,
        )
        .unwrap();
        assert_eq!(input.requires.as_deref(), Some("vision"));
        assert_eq!(input.max_cost, Some(0.01));
        assert!(!input.allow_cloud);
        assert!(
            parse_completion(
                br#"{"messages":[{"role":"user","content":"hi"}],"susi":{"max_cost":-1}}"#,
                false
            )
            .is_err()
        );
        assert!(
            parse_completion(
                br#"{"messages":[{"role":"user","content":"hi"}],"susi":{"allow_cloud":"no"}}"#,
                false,
            )
            .is_err()
        );
    }

    /// Drive one accepted connection through `negotiate_transport` after the
    /// client pre-writes `first` bytes for the loopback peek to classify.
    async fn negotiate_with_prefix(
        first: &[u8],
        remote: bool,
        require_tls_remote: bool,
    ) -> Option<dual_transport::MaybeTls> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let first = first.to_vec();
        let client = tokio::spawn(async move {
            let c = tokio::net::TcpStream::connect(addr).await.unwrap();
            c.writable().await.unwrap();
            c.try_write(&first).unwrap();
            c
        });
        let (stream, _) = listener.accept().await.unwrap();
        let out = negotiate_transport(stream, remote, None, require_tls_remote, "[test]").await;
        let _client = client.await.unwrap();
        out
    }

    #[test]
    fn anthropic_api_key_is_the_bearer_only_on_the_messages_route() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("k1"));
        assert_eq!(
            presented_credential(&headers, "/v1/messages").as_deref(),
            Some("Bearer k1")
        );
        assert_eq!(presented_credential(&headers, "/v1/chat/completions"), None);
        headers.insert(
            hyper::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer k2"),
        );
        assert_eq!(
            presented_credential(&headers, "/v1/messages").as_deref(),
            Some("Bearer k2"),
            "an explicit Authorization header wins"
        );
    }

    /// Serve one raw HTTP/1.1 request through `handle_gemi_request`, as a
    /// remote (non-loopback) peer, and return the raw reply.
    async fn serve_remote(request: &'static [u8]) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let workspace = Arc::new(std::env::temp_dir());
            let admission = Arc::new(tokio::sync::Semaphore::new(1));
            let peer: std::net::IpAddr = [203, 0, 113, 7].into();
            let svc = service_fn(move |req| {
                handle_gemi_request(req, Arc::clone(&workspace), peer, Arc::clone(&admission))
            });
            let _ = http_conn::connection_builder()
                .serve_connection(TokioIo::new(stream), svc)
                .await;
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client.write_all(request).await.unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        server.await.unwrap();
        reply
    }

    fn reply_json(reply: &str) -> serde_json::Value {
        let (_, body) = reply.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap()
    }

    #[tokio::test]
    async fn messages_route_rejects_unauthenticated_callers_in_the_anthropic_envelope() {
        let reply = serve_remote(
            b"POST /v1/messages HTTP/1.1\r\nHost: x\r\nConnection: close\r\nx-api-key: wrong\r\nContent-Length: 2\r\n\r\n{}",
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
        let body = reply_json(&reply);
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "authentication_error");

        let reply = serve_remote(
            b"POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}",
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
        let body = reply_json(&reply);
        assert_eq!(
            body["error"]["code"], "invalid_api_key",
            "OpenAI routes keep the OpenAI envelope"
        );
    }

    async fn dispatch_get(path: &str) -> (StatusCode, String, String) {
        let response = match dispatch_gemi_route(
            Method::GET,
            path.to_string(),
            Bytes::new(),
            Arc::new(std::env::temp_dir()),
            Arc::new(tokio::sync::Semaphore::new(2)),
        )
        .await
        {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = match response.into_body().collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(never) => match never {},
        };
        (
            status,
            content_type,
            String::from_utf8(bytes.to_vec()).unwrap(),
        )
    }

    /// B5: 9091 serves the OpenAI-compatible surface, the Anthropic
    /// translation, the Prometheus scrape and RFC 8615 discovery; the ad-hoc
    /// control-plane routes are gone (they are MCP tools now).
    #[tokio::test]
    async fn routes_serve_metrics_and_discovery_and_not_the_removed_control_plane() {
        let (status, content_type, body) = dispatch_get("/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, PROMETHEUS_CONTENT_TYPE);
        assert!(body.contains("# TYPE susi_up gauge"), "{body}");
        let (status, _, body) = dispatch_get("/.well-known/susi").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, _, body) = dispatch_get("/").await;
        assert_eq!(status, StatusCode::OK);
        let index: serde_json::Value = serde_json::from_str(&body).unwrap();
        let endpoints = index["endpoints"].to_string();
        for served in ["/v1/messages", "/metrics", "/.well-known/susi"] {
            assert!(
                endpoints.contains(served),
                "{served} missing from {endpoints}"
            );
        }
        assert!(!endpoints.contains("/runtime/"), "{endpoints}");
        for removed in [
            "/well-known/susi",
            "/runtime/models",
            "/runtime/placement",
            "/runtime/placement/decisions/placement-1-2-3",
            "/app",
            "/favicon.ico",
        ] {
            assert_eq!(
                dispatch_get(removed).await.0,
                StatusCode::NOT_FOUND,
                "{removed}"
            );
        }
    }

    #[tokio::test]
    async fn bounded_connection_builder_serves_requests() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let svc = service_fn(|_req| async {
                Ok::<_, Infallible>(json_response(StatusCode::OK, &json!({"ok": true})))
            });
            http_conn::connection_builder()
                .serve_connection(TokioIo::new(stream), svc)
                .await
                .unwrap();
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn silent_clients_are_dropped_after_the_negotiation_bound() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _silent = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let started = std::time::Instant::now();
        let out = dual_transport::bounded(
            std::time::Duration::from_millis(150),
            "[test]",
            dual_transport::negotiate_unbounded(stream, false, None, false, "[test]"),
        )
        .await;
        assert!(out.is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn negotiate_transport_serves_plain_http_on_loopback() {
        // ASCII 'G' of "GET" is not a TLS record — loopback plain HTTP proceeds.
        assert!(
            negotiate_with_prefix(b"GET /v1/models HTTP/1.1\r\n\r\n", false, false)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn negotiate_transport_drops_tls_bytes_without_acceptor() {
        // 0x16 is a TLS handshake record; with no configured acceptor the
        // loopback connection must be refused rather than served garbage.
        assert!(
            negotiate_with_prefix(&[0x16, 0x03, 0x01, 0x00, 0x2a], false, false)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn negotiate_transport_remote_plaintext_policy() {
        // Remote plaintext passes only when the daemon does not require
        // remote TLS (allow_remote_plaintext); requiring it refuses it. (The
        // remote-TLS handshake path needs a cert and is covered by live
        // verification.)
        assert!(
            negotiate_with_prefix(b"GET / HTTP/1.1\r\n\r\n", true, false)
                .await
                .is_some()
        );
        assert!(
            negotiate_with_prefix(b"GET / HTTP/1.1\r\n\r\n", true, true)
                .await
                .is_none()
        );
        // Loopback is always exempt — internal `http://127.0.0.1` callers
        // must not die when remote TLS is required.
        assert!(
            negotiate_with_prefix(b"GET / HTTP/1.1\r\n\r\n", false, true)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn api_errors_are_openai_shaped_with_rfc_headers() {
        let unauthorized = api_error(StatusCode::UNAUTHORIZED, "Unauthorized");
        assert_eq!(
            unauthorized.headers()[hyper::header::WWW_AUTHENTICATE],
            "Bearer realm=\"susi-gemi\""
        );
        let limited = api_error(StatusCode::TOO_MANY_REQUESTS, "slow down");
        assert_eq!(limited.headers()[hyper::header::RETRY_AFTER], "60");
        for (status, kind, code) in [
            (StatusCode::BAD_GATEWAY, "server_error", json!(null)),
            (
                StatusCode::UNAUTHORIZED,
                "invalid_request_error",
                json!("invalid_api_key"),
            ),
            (
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                json!(null),
            ),
        ] {
            let bytes = api_error(status, "msg")
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(v["error"]["type"], kind, "{status}");
            assert_eq!(v["error"]["code"], code, "{status}");
            assert_eq!(v["error"]["message"], "msg");
        }
    }

    #[test]
    fn embedding_input_is_strict_and_base64_is_le_f32() {
        assert_eq!(embedding_texts(&json!("a")).unwrap(), vec!["a"]);
        assert_eq!(embedding_texts(&json!(["a", "b"])).unwrap(), vec!["a", "b"]);
        assert!(
            embedding_texts(&json!([1, 2, 3]))
                .unwrap_err()
                .contains("token-array")
        );
        assert!(embedding_texts(&json!([[1, 2]])).is_err());
        assert!(embedding_texts(&json!(["a", null])).is_err());
        assert!(embedding_texts(&json!([])).is_err());
        assert!(embedding_texts(&json!("")).is_err());
        // 1.0f32 = 00 00 80 3F little-endian.
        assert_eq!(embedding_base64(&[1.0]), "AACAPw==");
    }

    #[test]
    fn cap_completion_enforces_word_and_char_budgets() {
        // No cap passes through unchanged.
        assert_eq!(cap_completion("a b c", None), ("a b c".into(), false));
        // Word budget binds first.
        let (text, cut) = cap_completion("one two three four five", Some(2));
        assert!(cut);
        assert_eq!(text, "one two");
        // Char budget (4/token) binds for dense single-token-ish text.
        let (text, cut) = cap_completion("abcdefghij", Some(2));
        assert!(cut);
        assert!(text.len() <= 8);
        // Under budget is untouched.
        assert_eq!(cap_completion("short", Some(10)), ("short".into(), false));
    }

    #[test]
    fn parses_max_tokens_and_rejects_invalid() {
        let input = parse_completion(
            br#"{"messages":[{"role":"user","content":"hi"}],"max_tokens":42}"#,
            false,
        )
        .unwrap();
        assert_eq!(input.max_tokens, Some(42));
        let aliased = parse_completion(
            br#"{"messages":[{"role":"user","content":"hi"}],"max_completion_tokens":7}"#,
            false,
        )
        .unwrap();
        assert_eq!(aliased.max_tokens, Some(7));
        assert!(
            parse_completion(
                br#"{"messages":[{"role":"user","content":"hi"}],"max_tokens":-3}"#,
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn parses_stop_sequences_and_rejects_multi_n() {
        let input = parse_completion(
            br#"{"messages":[{"role":"user","content":"hi"}],"stop":["\n\n","END"]}"#,
            false,
        )
        .unwrap();
        assert_eq!(input.stop, vec!["\n\n".to_string(), "END".to_string()]);
        // A bare string stop is accepted; null disables; empty strings are dropped.
        let input = parse_completion(
            br#"{"messages":[{"role":"user","content":"hi"}],"stop":"HALT"}"#,
            false,
        )
        .unwrap();
        assert_eq!(input.stop, vec!["HALT".to_string()]);
        let input = parse_completion(
            br#"{"messages":[{"role":"user","content":"hi"}],"stop":[""]}"#,
            false,
        )
        .unwrap();
        assert!(input.stop.is_empty());
        assert!(
            parse_completion(
                br#"{"messages":[{"role":"user","content":"hi"}],"stop":42}"#,
                false,
            )
            .is_err()
        );
        assert!(
            parse_completion(
                br#"{"messages":[{"role":"user","content":"hi"}],"n":2}"#,
                false,
            )
            .is_err()
        );
        assert!(
            parse_completion(
                br#"{"messages":[{"role":"user","content":"hi"}],"n":1}"#,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn stop_sequences_truncate_and_hold_partial_matches() {
        let stops = vec!["END".to_string(), "\n\n".to_string()];
        assert_eq!(first_stop("hello END world", &stops), Some(6));
        assert_eq!(apply_stops("a\n\nb", &stops), "a");
        assert_eq!(apply_stops("no stops here", &stops), "no stops here");
        // A chunk ending in a proper stop prefix must hold those bytes back.
        assert_eq!(partial_stop_hold("text EN", &stops), 2);
        assert_eq!(partial_stop_hold("text\n", &stops), 1);
        assert_eq!(partial_stop_hold("plain", &stops), 0);
        // The full stop itself is not held — it was already matched.
        assert_eq!(partial_stop_hold("all END", &stops), 0);
    }

    #[test]
    fn inference_render_unwraps_to_raw_model_output() {
        let rendered = "Source: inference:local | receipt 1-2:0 | observed at Unix 9 | arguments {\"goal\":\"g\"} | selector entire response | output_hash abc\nTool reported:\n> ok\n> second line";
        assert_eq!(
            unwrap_inference_render(rendered).as_deref(),
            Some("ok\nsecond line")
        );
        // Tool receipts and non-rendered answers are not unwrapped.
        assert!(
            unwrap_inference_render("Source: status | receipt 1:0 | \nTool reported:\n> x")
                .is_none()
        );
        assert!(unwrap_inference_render("plain answer").is_none());
    }

    #[test]
    fn preserves_conversation_and_all_text_parts() {
        let input = parse_completion(br#"{"messages":[{"role":"system","content":"Be brief"},{"role":"user","content":"Remember 42"},{"role":"assistant","content":"OK"},{"role":"user","content":[{"type":"text","text":"What "},{"type":"text","text":"number?"}]}],"stream": true}"#, false).unwrap();
        // Multi-turn requests carry their roles to the GEMI plane's
        // cache-first assembly — the flatten is its job now.
        assert_eq!(
            input.turns.as_deref(),
            Some(
                [
                    ("system".to_string(), "Be brief".to_string()),
                    ("user".to_string(), "Remember 42".to_string()),
                    ("assistant".to_string(), "OK".to_string()),
                    ("user".to_string(), "What number?".to_string()),
                ]
                .as_slice()
            )
        );
        assert!(input.stream);
    }

    #[test]
    fn assemble_context_prompt_falls_back_to_literal_join() {
        // With no GEMI plane wired (unit-test topology) the join is the
        // same literal order the endpoint always produced.
        let turns = vec![
            ("system".to_string(), "Be brief".to_string()),
            ("user".to_string(), "Remember 42".to_string()),
            ("assistant".to_string(), "OK".to_string()),
            ("user".to_string(), "What number?".to_string()),
        ];
        assert_eq!(
            assemble_context_prompt(&turns, None),
            "system: Be brief\n\nuser: Remember 42\n\nassistant: OK\n\nuser: What number?"
        );
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
        let mut rx = completion_stream(
            "quoted\"model".into(),
            false,
            None,
            Vec::new(),
            false,
            0,
            None,
            |_, _| "Hello 🦀".into(),
        );
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
    async fn streaming_placement_rides_only_the_opening_frame() {
        let mut rx = completion_stream(
            "m".into(),
            false,
            None,
            Vec::new(),
            false,
            0,
            Some(placement_value("placement-test-3", "cloud")),
            |_, _| "answer".into(),
        );
        let mut frames = Vec::new();
        while let Some(frame) = rx.recv().await {
            if frame == "data: [DONE]\n\n" {
                break;
            }
            let value: serde_json::Value =
                serde_json::from_str(frame.strip_prefix("data: ").unwrap().trim()).unwrap();
            frames.push(value);
        }
        assert_eq!(frames[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(
            frames[0]["susi_placement"],
            json!({"decision_id": "placement-test-3", "target": "cloud"})
        );
        assert!(
            frames[1..]
                .iter()
                .all(|f| f.get("susi_placement").is_none())
        );
    }

    #[tokio::test]
    async fn streaming_does_not_duplicate_callback_output_and_chunks_unicode() {
        let answer = "🦀".repeat(3000);
        let expected = answer.clone();
        let mut rx = completion_stream(
            "model".into(),
            true,
            None,
            Vec::new(),
            false,
            0,
            None,
            move |callback, _| {
                callback(answer.clone());
                answer
            },
        );
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
        let first = completion_response("m", "answer", true, "stop");
        let second = completion_response("m", "answer", false, "stop");
        assert_ne!(first["id"], second["id"]);
        assert!(first["created"].as_u64().unwrap().abs_diff(now_secs()) <= 1);
        assert_eq!(first["choices"][0]["text"], "answer");
        assert_eq!(second["choices"][0]["message"]["content"], "answer");
    }
    #[tokio::test]
    async fn streaming_include_usage_emits_terminal_usage_chunk() {
        let mut rx = completion_stream(
            "m".into(),
            false,
            None,
            Vec::new(),
            true,
            4,
            None,
            move |callback, _| {
                callback("hello there world".to_string());
                String::new()
            },
        );
        let mut usage: Option<serde_json::Value> = None;
        let mut saw_finish = false;
        while let Some(frame) = rx.recv().await {
            if frame == "data: [DONE]\n\n" {
                break;
            }
            let value: serde_json::Value = serde_json::from_str(frame[6..].trim()).unwrap();
            if let Some(u) = value.get("usage") {
                // The usage chunk must come after the finish chunk and
                // carry an empty choices array per the OpenAI contract.
                assert!(saw_finish);
                assert_eq!(value["choices"].as_array().unwrap().len(), 0);
                usage = Some(u.clone());
            }
            saw_finish |= value["choices"][0]["finish_reason"] == "stop";
        }
        let u = usage.expect("include_usage must emit a usage chunk");
        assert_eq!(u["prompt_tokens"], 4);
        // 17 chars/4 = 4, 3 words -> max(4, 3) = 4
        assert_eq!(u["completion_tokens"], 4);
        assert_eq!(u["total_tokens"], 8);
        assert_eq!(u["estimated"], true);
    }

    #[tokio::test]
    async fn disconnected_stream_releases_admission() {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = admission.clone().try_acquire_owned().unwrap();
        let rx = completion_stream(
            "m".into(),
            false,
            None,
            Vec::new(),
            false,
            0,
            None,
            move |_, _| {
                let _permit = permit;
                "answer".into()
            },
        );
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
        let rx = completion_stream(
            "m".into(),
            false,
            None,
            Vec::new(),
            false,
            0,
            None,
            move |callback, _| {
                let _permit = permit;
                let _ = started_tx.send(());
                callback("x".repeat(4096 * 32));
                "".into()
            },
        );
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

    #[test]
    fn test_parse_completion_with_tool_role_and_tool_calls() {
        let json_body = br#"{
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are a code assistant."},
                {"role": "user", "content": "Call tool test"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "run_test", "arguments": "{\"suite\":\"unit\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "test passed"}
            ]
        }"#;

        let res = parse_completion(json_body, false);
        assert!(res.is_ok(), "failed to parse: {:?}", res.err());
        let input = res.unwrap();
        assert_eq!(input.model.as_deref(), Some("gpt-4o"));
        let turns = input.turns.expect("expected multi-turn parsed");
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0].0, "system");
        assert_eq!(turns[1].0, "user");
        assert_eq!(turns[2].0, "assistant");
        assert!(turns[2].1.contains("run_test"));
        assert_eq!(turns[3].0, "tool");
        assert!(turns[3].1.contains("[tool_call_id: call_1]"));
    }

    #[test]
    fn test_openai_model_list_contains_ide_proxy_defaults() {
        let scratch = std::env::temp_dir();
        let models = openai_model_list(&scratch);
        let ids: Vec<&str> = models
            .iter()
            .filter_map(|m| m.get("id").and_then(|id| id.as_str()))
            .collect();
        assert!(
            ids.contains(&"susi"),
            "expected 'susi' in model list: {:?}",
            ids
        );
        assert!(
            ids.contains(&"auto"),
            "expected 'auto' in model list: {:?}",
            ids
        );
        assert!(
            ids.contains(&"gpt-4o"),
            "expected 'gpt-4o' in model list: {:?}",
            ids
        );
    }
}
