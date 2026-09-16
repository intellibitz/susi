// GEMI HTTP REST Substrate: OpenAI-Compatible Interface & Adaptive Web Interface
// 100% Rust implementation serving Tier 1 & Tier 2 Intelligence Swarms
use rayon::prelude::*;

use serde_json::json;
use std::path::PathBuf;
use std::thread;
use tiny_http::{Header, Method, Response, Server};

use crate::gawd::ama::SusiMasterAgent;
use crate::gemi::models::ModelManager;
use crate::gmcp::tools::ToolRegistry;

pub struct GemiServer;

impl GemiServer {
    pub fn start_http_server(workspace: PathBuf, server: Server) {
        let addr = server.server_addr().to_string();
        eprintln!("[GEMI REST] Substrate active on {}", addr);
        eprintln!(
            "[GEMI Web] UI Interface: http://localhost:{}/app",
            server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0)
        );

        for mut request in server.incoming_requests() {
            let workspace = workspace.clone();
            let method = request.method().clone();
            let url = request.url().to_string();

            let mut body_str = String::new();
            let _ = std::io::Read::read_to_string(request.as_reader(), &mut body_str);

            let workspace_thread = workspace.clone();
            let method_thread = method.clone();
            let url_thread = url.clone();
            let body_thread = body_str.clone();

            thread::spawn(move || {
                // Change type to Box<dyn Read> to support both memory and streaming responses
                let (tx, rx) =
                    flume::bounded::<Result<Response<Box<dyn std::io::Read + Send>>, String>>(1);
                let w_thread = workspace_thread;
                let m_thread = method_thread;
                let u_thread = url_thread;
                let b_thread = body_thread;

                thread::spawn(move || {
                    let result = match (m_thread, u_thread.as_str()) {
                        (
                            Method::Get,
                            "/" | "/v1" | "/v1/" | "/health" | "/app" | "/favicon.ico",
                        ) => {
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
                            let payload =
                                serde_json::to_string_pretty(&api_status).unwrap_or_default();
                            Ok(Response::empty(200)
                                .with_data(
                                    Box::new(std::io::Cursor::new(payload.into_bytes()))
                                        as Box<dyn std::io::Read + Send>,
                                    None,
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Content-Type"[..],
                                        &b"application/json"[..],
                                    )
                                    .unwrap(),
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Access-Control-Allow-Origin"[..],
                                        &b"*"[..],
                                    )
                                    .unwrap(),
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Access-Control-Allow-Headers"[..],
                                        &b"*"[..],
                                    )
                                    .unwrap(),
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Access-Control-Allow-Methods"[..],
                                        &b"GET, POST, OPTIONS"[..],
                                    )
                                    .unwrap(),
                                ))
                        }
                        (Method::Get, path)
                            if path.starts_with("/v1/models") || path.starts_with("/models") =>
                        {
                            let models = ModelManager::list_models(&w_thread);
                            // PARALLEL PROCESSING MANDATE
                            let json_models: Vec<serde_json::Value> = models.par_iter()
                                .map(|m| json!({"id": &m.model_id(), "object": "model", "owned_by": "susi"}))
                                .collect();
                            let payload_val = json!({"object": "list", "data": json_models});
                            let payload = serde_json::to_string(&payload_val).unwrap_or_default();

                            Ok(Response::empty(200)
                                .with_data(
                                    Box::new(std::io::Cursor::new(payload.into_bytes()))
                                        as Box<dyn std::io::Read + Send>,
                                    None,
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Content-Type"[..],
                                        &b"application/json"[..],
                                    )
                                    .unwrap(),
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Access-Control-Allow-Origin"[..],
                                        &b"*"[..],
                                    )
                                    .unwrap(),
                                ))
                        }
                        (Method::Get, "/well-known/susi") => {
                            let hardware = crate::gemi::hardware::HardwareProfiler::get_profile();
                            let (engine, model) =
                                crate::gemi::models::ModelManager::get_active_engine_and_model();
                            let tools = ToolRegistry::list_tools();

                            let info = json!({
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
                            });

                            let payload = serde_json::to_string_pretty(&info).unwrap_or_default();
                            Ok(Response::empty(200)
                                .with_data(
                                    Box::new(std::io::Cursor::new(payload.into_bytes()))
                                        as Box<dyn std::io::Read + Send>,
                                    None,
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Content-Type"[..],
                                        &b"application/json"[..],
                                    )
                                    .unwrap(),
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Access-Control-Allow-Origin"[..],
                                        &b"*"[..],
                                    )
                                    .unwrap(),
                                ))
                        }
                        (Method::Post, path)
                            if path.starts_with("/v1/chat/completions")
                                || path.starts_with("/chat/completions")
                                || path.starts_with("/v1/completions")
                                || path == "/"
                                || path == "/v1"
                                || path == "/v1/" =>
                        {
                            let is_streaming = b_thread.contains("\"stream\":true")
                                || b_thread.contains("\"stream\": true")
                                || b_thread.contains("stream");
                            let active_model =
                                crate::gemi::models::ModelManager::get_selected_model()
                                    .unwrap_or_else(|| "susi-native-synthesis".to_string());
                            let model_name = active_model.as_str();

                            let pulse_intent = extract_prompt_from_json(&b_thread)
                                .unwrap_or_else(|| "list workspace health".to_string());
                            crate::sandbox::manager::SusiAuditLogger::log_event(
                                &w_thread,
                                "WEB_MISSION_START",
                                &pulse_intent,
                            );

                            let trimmed_prompt = pulse_intent.trim();
                            let clean_cmd = trimmed_prompt
                                .trim_start_matches('/')
                                .trim_start_matches(':');
                            let parts: Vec<&str> = clean_cmd.splitn(2, ' ').collect();
                            let tool_name = parts[0].to_lowercase();
                            let tool_arg = parts.get(1).copied().unwrap_or("").trim();

                            if is_streaming {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                let m_name = model_name.to_string();
                                let prompt_clone = trimmed_prompt.to_string();
                                let w_clone = w_thread.clone();
                                let _tool_name_clone = tool_name.clone();
                                let _tool_arg_clone = tool_arg.to_string();

                                let (chunk_tx, chunk_rx) = flume::unbounded::<String>();

                                // DYNAMIC, NON-BLOCKING, MULTI-THREADED CONCURRENT STREAMING
                                rayon::spawn(move || {
                                    let _ = chunk_tx.send(format!("data: {{\"id\":\"chatcmpl-susi-{now}\",\"object\":\"chat.completion.chunk\",\"created\":{now},\"model\":\"{m_name}\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\"}},\"finish_reason\":null}}]}}\n\n"));

                                    let ama = SusiMasterAgent::new();
                                    let _ = ama.solve_stream(&prompt_clone, &w_clone, crate::SUSI_VERSION, &|piece| {
                                        let _ = chunk_tx.send(format!("data: {{\"id\":\"chatcmpl-susi-{now}\",\"object\":\"chat.completion.chunk\",\"created\":{now},\"model\":\"{m_name}\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{json_piece}}},\"finish_reason\":null}}]}}\n\n", json_piece=serde_json::to_string(&piece).unwrap_or_default()));
                                    });
                                    let _ = chunk_tx.send(format!("data: {{\"id\":\"chatcmpl-susi-{now}\",\"object\":\"chat.completion.chunk\",\"created\":{now},\"model\":\"{m_name}\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n"));
                                    let _ = chunk_tx.send("data: [DONE]\n\n".to_string());
                                });

                                struct StreamAdapter(flume::Receiver<String>, Vec<u8>);
                                impl std::io::Read for StreamAdapter {
                                    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                                        if self.1.is_empty() {
                                            match self.0.recv() {
                                                Ok(s) => self.1.extend_from_slice(s.as_bytes()),
                                                Err(_) => return Ok(0),
                                            }
                                        }
                                        let len = std::cmp::min(buf.len(), self.1.len());
                                        buf[..len].copy_from_slice(&self.1[..len]);
                                        self.1.drain(..len);
                                        Ok(len)
                                    }
                                }

                                Ok(Response::empty(200)
                                    .with_data(
                                        Box::new(StreamAdapter(chunk_rx, vec![]))
                                            as Box<dyn std::io::Read + Send>,
                                        None,
                                    )
                                    .with_header(
                                        Header::from_bytes(
                                            &b"Content-Type"[..],
                                            &b"text/event-stream"[..],
                                        )
                                        .unwrap(),
                                    )
                                    .with_header(
                                        Header::from_bytes(
                                            &b"Access-Control-Allow-Origin"[..],
                                            &b"*"[..],
                                        )
                                        .unwrap(),
                                    ))
                            } else {
                                let content = if ToolRegistry::exists(&tool_name) {
                                    ToolRegistry::execute_tool(
                                        &tool_name,
                                        &serde_json::json!(tool_arg),
                                        &w_thread,
                                    )
                                } else {
                                    let ama = SusiMasterAgent::new();
                                    let final_resp = ama.solve_clean(
                                        trimmed_prompt,
                                        &w_thread,
                                        crate::SUSI_VERSION,
                                    );
                                    crate::sandbox::manager::SusiMemory::save_interaction(
                                        &w_thread,
                                        trimmed_prompt,
                                        &final_resp,
                                    );
                                    final_resp
                                };

                                let payload = json!({
                                    "id": format!("chatcmpl-susi-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)),
                                    "object": "chat.completion",
                                    "created": 1700000000,
                                    "model": model_name,
                                    "choices": [{
                                        "index": 0,
                                        "message": { "role": "assistant", "content": content },
                                        "finish_reason": "stop"
                                    }]
                                });

                                Ok(Response::empty(200)
                                    .with_data(
                                        Box::new(std::io::Cursor::new(
                                            serde_json::to_string(&payload)
                                                .unwrap_or_default()
                                                .into_bytes(),
                                        ))
                                            as Box<dyn std::io::Read + Send>,
                                        None,
                                    )
                                    .with_header(
                                        Header::from_bytes(
                                            &b"Content-Type"[..],
                                            &b"application/json"[..],
                                        )
                                        .unwrap(),
                                    )
                                    .with_header(
                                        Header::from_bytes(
                                            &b"Access-Control-Allow-Origin"[..],
                                            &b"*"[..],
                                        )
                                        .unwrap(),
                                    ))
                            }
                        }
                        (Method::Options, _) => Ok(Response::empty(200)
                            .with_data(
                                Box::new(std::io::Cursor::new(vec![]))
                                    as Box<dyn std::io::Read + Send>,
                                None,
                            )
                            .with_header(
                                Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..])
                                    .unwrap(),
                            )
                            .with_header(
                                Header::from_bytes(
                                    &b"Access-Control-Allow-Methods"[..],
                                    &b"GET, POST, OPTIONS"[..],
                                )
                                .unwrap(),
                            )
                            .with_header(
                                Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"*"[..])
                                    .unwrap(),
                            )),
                        _ => {
                            let payload = json!({"error": "Endpoint not found"}).to_string();
                            Ok(Response::empty(404)
                                .with_data(
                                    Box::new(std::io::Cursor::new(payload.into_bytes()))
                                        as Box<dyn std::io::Read + Send>,
                                    None,
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Content-Type"[..],
                                        &b"application/json"[..],
                                    )
                                    .unwrap(),
                                )
                                .with_header(
                                    Header::from_bytes(
                                        &b"Access-Control-Allow-Origin"[..],
                                        &b"*"[..],
                                    )
                                    .unwrap(),
                                ))
                        }
                    };
                    let _ = tx.send(result);
                });

                let response = rx.recv().unwrap_or_else(|_| {
                    let payload = json!({
                        "error": "Mission Interrupted",
                        "message": "The intelligence substrate mission was cancelled or failed."
                    })
                    .to_string();
                    Ok(Response::empty(500)
                        .with_data(
                            Box::new(std::io::Cursor::new(payload.into_bytes()))
                                as Box<dyn std::io::Read + Send>,
                            None,
                        )
                        .with_header(
                            Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                                .unwrap(),
                        ))
                });

                if let Ok(resp) = response {
                    let _ = request.respond(resp);
                }
            });
        }
    }
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
