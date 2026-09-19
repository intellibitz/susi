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
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use rayon::prelude::*;
use serde_json::json;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

use crate::gawd::ama::SusiMasterAgent;
use crate::gemi::models::ModelManager;
use crate::gmcp::tools::ToolRegistry;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn full_body<T: Into<Bytes>>(chunk: T) -> BoxBody {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

fn json_response(status: StatusCode, payload: &serde_json::Value) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .header(
            "Access-Control-Allow-Origin",
            HeaderValue::from_str(
                &crate::sandbox::manager::SusiConfig::load_global()
                    .unwrap_or_default()
                    .get("allow_origin")
                    .unwrap_or_else(|| "*".to_string()),
            )
            .unwrap_or_else(|_| HeaderValue::from_static("*")),
        )
        .body(full_body(
            serde_json::to_string(payload).unwrap_or_default(),
        ))
        .unwrap()
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

                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| {
                        let workspace = Arc::clone(&workspace);
                        async move { handle_gemi_request(req, workspace, peer_ip).await }
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

async fn handle_gemi_request(
    req: Request<Incoming>,
    workspace: Arc<PathBuf>,
    peer_ip: std::net::IpAddr,
) -> Result<Response<BoxBody>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    // CORS preflight never carries an Authorization header, and /health is a
    // conventional unauthenticated liveness probe — everything else on this
    // world-facing surface is gated below.
    if method != Method::OPTIONS && path != "/health" {
        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        if !crate::gawd::net_guard::NetGuard::is_authorized(
            req.headers()
                .get(hyper::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
        ) {
            return Ok(json_response(
                StatusCode::UNAUTHORIZED,
                &json!({"error": "Unauthorized"}),
            ));
        }
        if !crate::gawd::net_guard::RateLimiter::global()
            .check(peer_ip, cfg.rate_limit_per_minute())
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
                "version": crate::SUSI_VERSION,
                "status": "active",
                "endpoints": [
                    "/v1/chat/completions",
                    "/v1/models",
                    "/v1/completions"
                ]
            });
            Ok(json_response(StatusCode::OK, &api_status))
        }
        (&Method::GET, p) if p.starts_with("/v1/models") || p.starts_with("/models") => {
            let ws = (*workspace).clone();
            let payload = tokio::task::spawn_blocking(move || {
                let models = ModelManager::list_models(&ws);
                let json_models: Vec<serde_json::Value> = models
                    .par_iter()
                    .map(|m| json!({"id": m.model_id(), "object": "model", "owned_by": "susi"}))
                    .collect();
                json!({"object": "list", "data": json_models})
            })
            .await
            .unwrap_or_else(|_| json!({"object": "list", "data": []}));
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::GET, "/well-known/susi") => {
            let payload = tokio::task::spawn_blocking(move || {
                let hardware = crate::gemi::hardware::HardwareProfiler::get_profile();
                let (engine, model) = ModelManager::get_active_engine_and_model(None);
                let tools = ToolRegistry::list_tools();
                json!({
                    "version": crate::SUSI_VERSION,
                    "identity": "SUSI Intelligence Substrate",
                    "engine": engine,
                    "model": model,
                    "hardware": {
                        "cpus": hardware.cpus,
                        "gpu": hardware.gpu_info,
                        "acceleration": hardware.acceleration_active,
                        "os": hardware.os_info
                    },
                    "reflexes": tools.iter().map(|t| &t.name).collect::<Vec<_>>()
                })
            })
            .await
            .unwrap_or_else(|_| json!({"error": "identity introspection failed"}));
            Ok(json_response(StatusCode::OK, &payload))
        }
        (&Method::POST, p)
            if p.starts_with("/v1/chat/completions")
                || p.starts_with("/chat/completions")
                || p.starts_with("/v1/completions")
                || p == "/"
                || p == "/v1"
                || p == "/v1/" =>
        {
            // H9: Limit request body to 10MB to prevent OOM DOS
            use http_body_util::BodyExt;
            let limited_body = http_body_util::Limited::new(req.into_body(), 10 * 1024 * 1024);
            let body_bytes = limited_body
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            let is_streaming =
                body_str.contains("\"stream\":true") || body_str.contains("\"stream\": true");

            let pulse_intent = extract_prompt_from_json(&body_str)
                .unwrap_or_else(|| "list workspace health".to_string());
            let active_model = ModelManager::get_selected_model(Some(
                crate::gemi::intent::IntentClassifier::classify(&pulse_intent),
            ))
            .unwrap_or_else(|| "susi-native-synthesis".to_string());
            crate::sandbox::manager::SusiAuditLogger::log_event(
                &workspace,
                "WEB_MISSION_START",
                &pulse_intent,
            );
            let trimmed_prompt = pulse_intent.trim().to_string();

            if is_streaming {
                Ok(build_streaming_response(
                    trimmed_prompt,
                    active_model,
                    Arc::clone(&workspace),
                ))
            } else {
                let ws = (*workspace).clone();
                let prompt_for_task = trimmed_prompt.clone();
                let content = tokio::task::spawn_blocking(move || {
                    let ama = SusiMasterAgent::new();
                    let final_resp = ama.solve_clean(&prompt_for_task, &ws, crate::SUSI_VERSION);
                    crate::sandbox::manager::SusiMemory::save_interaction(
                        &ws,
                        &prompt_for_task,
                        &final_resp,
                        crate::SUSI_VERSION,
                    );
                    final_resp
                })
                .await
                .unwrap_or_else(|e| format!("SUSI Engine Error: task join failed: {}", e));

                let payload = json!({
                    "id": format!("chatcmpl-susi-{}", now_secs()),
                    "object": "chat.completion",
                    "created": 1700000000,
                    "model": active_model,
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": content },
                        "finish_reason": "stop"
                    }]
                });
                Ok(json_response(StatusCode::OK, &payload))
            }
        }
        (&Method::OPTIONS, _) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(
                "Access-Control-Allow-Origin",
                HeaderValue::from_str(
                    &crate::sandbox::manager::SusiConfig::load_global()
                        .unwrap_or_default()
                        .get("allow_origin")
                        .unwrap_or_else(|| "*".to_string()),
                )
                .unwrap_or_else(|_| HeaderValue::from_static("*")),
            )
            .header(
                "Access-Control-Allow-Methods",
                HeaderValue::from_static("GET, POST, OPTIONS"),
            )
            .header(
                "Access-Control-Allow-Headers",
                HeaderValue::from_str(
                    &crate::sandbox::manager::SusiConfig::load_global()
                        .unwrap_or_default()
                        .get("allow_origin")
                        .unwrap_or_else(|| "*".to_string()),
                )
                .unwrap_or_else(|_| HeaderValue::from_static("*")),
            )
            .body(full_body(Vec::new()))
            .unwrap()),
        _ => Ok(json_response(
            StatusCode::NOT_FOUND,
            &json!({"error": "Endpoint not found"}),
        )),
    }
}

fn build_streaming_response(
    prompt: String,
    model_name: String,
    workspace: Arc<PathBuf>,
) -> Response<BoxBody> {
    let now = now_secs();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // SusiMasterAgent::solve_stream is synchronous, CPU-bound swarm execution;
    // it must run on the blocking pool, not a tokio worker thread. Its
    // per-token callback feeds the unbounded channel, which is a non-blocking
    // send safe to call from that synchronous context.
    let m_name = model_name.clone();
    tokio::task::spawn_blocking(move || {
        let _ = tx.send(format!(
            "data: {{\"id\":\"chatcmpl-susi-{now}\",\"object\":\"chat.completion.chunk\",\"created\":{now},\"model\":\"{m_name}\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\"}},\"finish_reason\":null}}]}}\n\n"
        ));

        let ama = SusiMasterAgent::new();
        let _ = ama.solve_stream(&prompt, &workspace, crate::SUSI_VERSION, &|piece| {
            let json_piece = serde_json::to_string(&piece).unwrap_or_default();
            let _ = tx.send(format!(
                "data: {{\"id\":\"chatcmpl-susi-{now}\",\"object\":\"chat.completion.chunk\",\"created\":{now},\"model\":\"{m_name}\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{json_piece}}},\"finish_reason\":null}}]}}\n\n"
            ));
        });

        let _ = tx.send(format!(
            "data: {{\"id\":\"chatcmpl-susi-{now}\",\"object\":\"chat.completion.chunk\",\"created\":{now},\"model\":\"{m_name}\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n"
        ));
        let _ = tx.send("data: [DONE]\n\n".to_string());
    });

    let stream = UnboundedReceiverStream::new(rx)
        .map(|chunk| Ok::<_, Infallible>(Frame::data(Bytes::from(chunk))));
    let body = StreamBody::new(stream).boxed();

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header(
            "Access-Control-Allow-Origin",
            HeaderValue::from_str(
                &crate::sandbox::manager::SusiConfig::load_global()
                    .unwrap_or_default()
                    .get("allow_origin")
                    .unwrap_or_else(|| "*".to_string()),
            )
            .unwrap_or_else(|_| HeaderValue::from_static("*")),
        )
        .body(body)
        .unwrap()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn extract_prompt_from_json(body: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(messages) = v.get("messages").and_then(|m| m.as_array()) {
            if let Some(last) = messages.last() {
                if let Some(c) = last.get("content") {
                    if let Some(s) = c.as_str() {
                        return Some(s.to_string());
                    } else if let Some(arr) = c.as_array() {
                        for item in arr {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                return Some(text.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}
