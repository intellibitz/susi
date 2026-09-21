//! MCP stdio and Streamable HTTP entry points backed by the official Rust SDK.
use crate::protocol::GmcpService;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming,
    header::{HeaderValue, CONTENT_TYPE},
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
};
use rmcp::{
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ServiceExt,
};
use std::{
    convert::Infallible,
    path::{Path, PathBuf},
    sync::Arc,
};
use tower_service::Service;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;
type HttpService = StreamableHttpService<GmcpService, LocalSessionManager>;

fn response(status: StatusCode, text: &'static str) -> Response<BoxBody> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())).boxed());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    response
}

fn cors(response: &mut Response<BoxBody>, origin: &str) {
    if let Ok(value) = HeaderValue::from_str(origin) {
        response
            .headers_mut()
            .insert("access-control-allow-origin", value);
    }
    response.headers_mut().insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
    );
    response.headers_mut().insert("access-control-allow-headers", HeaderValue::from_static("Content-Type, Authorization, Accept, MCP-Protocol-Version, Mcp-Session-Id, Mcp-Method, Mcp-Name, Last-Event-ID"));
    response.headers_mut().insert(
        "access-control-expose-headers",
        HeaderValue::from_static("Mcp-Session-Id, MCP-Protocol-Version"),
    );
}

pub struct GmcpServer;
impl GmcpServer {
    pub fn run_stdio(workspace: &Path, version: &str) {
        eprintln!("[GMCP] v{version}: MCP over newline-delimited stdio");
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[GMCP] Runtime failed: {e}");
                return;
            }
        };
        let service = GmcpService::new(workspace.to_path_buf());
        if let Err(e) = crate::catalog::install(&service) {
            eprintln!("[GMCP] Catalog failed: {e}");
            return;
        }
        runtime.block_on(async move {
            let limit = susi_sandbox::manager::SusiConfig::load_global()
                .unwrap_or_default()
                .max_rpc_body_bytes();
            let transport = (
                crate::stdio::BoundedLines::new(tokio::io::stdin(), limit),
                tokio::io::stdout(),
            );
            match service.serve(transport).await {
                Ok(running) => {
                    if let Err(e) = running.waiting().await {
                        eprintln!("[GMCP] Stdio failed: {e}");
                    }
                }
                Err(e) => eprintln!("[GMCP] Stdio negotiation failed: {e}"),
            }
        });
    }

    pub fn start_http_server(workspace: PathBuf, listener: std::net::TcpListener) {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[GMCP] Runtime failed: {e}");
                return;
            }
        };
        runtime.block_on(async move {
            if let Err(e) = listener.set_nonblocking(true) {
                eprintln!("[GMCP] Listener failed: {e}");
                return;
            }
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[GMCP] Listener failed: {e}");
                    return;
                }
            };
            let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
            let permits = Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrent_agents()));
            let application = GmcpService::new(workspace);
            if let Err(e) = crate::catalog::install(&application) {
                eprintln!("[GMCP] Catalog failed: {e}");
                return;
            }
            let service = http_service(application);
            eprintln!("[GMCP] Streamable HTTP available at /mcp (alias /messages)");
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("[GMCP] Accept failed: {e}");
                        continue;
                    }
                };
                let permit = match permits.clone().try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => {
                        drop(stream);
                        continue;
                    }
                };
                let service = service.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let svc =
                        service_fn(move |req| handle_request(req, service.clone(), peer.ip()));
                    if let Err(e) = Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(stream), svc)
                        .await
                    {
                        eprintln!("[GMCP] Connection failed: {e}");
                    }
                });
            }
        });
    }
}

pub fn http_service(service: GmcpService) -> HttpService {
    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let mut transport = StreamableHttpServerConfig::default();
    transport.max_request_body_bytes = cfg.max_rpc_body_bytes();
    if let Some(hosts) = cfg.get::<Vec<String>>("mcp_allowed_hosts") {
        transport = transport.with_allowed_hosts(hosts);
    }
    let origin = cfg.allow_origin();
    // Wildcard CORS is not an Origin-validation policy. With no explicit origin
    // configured, reject browser origins; non-browser clients still pass.
    transport = if origin == "*" {
        transport.enforce_origin_validation()
    } else {
        transport.with_allowed_origins([origin])
    };
    StreamableHttpService::new(
        move || Ok(service.session()),
        Arc::new(LocalSessionManager::default()),
        transport,
    )
}

async fn handle_request(
    req: Request<Incoming>,
    mut service: HttpService,
    peer: std::net::IpAddr,
) -> Result<Response<BoxBody>, Infallible> {
    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let mut result = if req.method() == Method::OPTIONS {
        response(StatusCode::NO_CONTENT, "")
    } else if !susi_agents::net_guard::NetGuard::is_authorized(
        req.headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        peer,
    ) {
        response(StatusCode::UNAUTHORIZED, "Unauthorized")
    } else if !susi_agents::net_guard::RateLimiter::global()
        .check(peer, cfg.rate_limit_per_minute())
    {
        response(StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded")
    } else if matches!(req.uri().path(), "/mcp" | "/messages") {
        service.call(req).await?
    } else {
        response(
            StatusCode::NOT_FOUND,
            "Use the MCP Streamable HTTP endpoint at /mcp",
        )
    };
    cors(&mut result, &cfg.allow_origin());
    Ok(result)
}
