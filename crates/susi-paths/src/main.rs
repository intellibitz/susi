//! Standalone `susi-paths` REST service.
//!
//! Serves the host substrate path/port contract over HTTP so decoupled crates
//! can resolve `SusiDirs`/`ports` without a source dependency on this crate.
//! Default bind: `127.0.0.1:18080` (override with `SUSI_PATHS_PORT`).

use axum::{routing::get, Json, Router};
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use susi_paths::SusiDirs;

const DEFAULT_PORT: u16 = 18080;

#[derive(Serialize)]
struct PathsResponse {
    home_dir: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    substrate_home: PathBuf,
}

#[derive(Serialize)]
struct PortsResponse {
    gmcp: u16,
    gemi: u16,
    udp_discovery: u16,
    gmcp_http: u16,
}

async fn get_paths() -> Json<PathsResponse> {
    Json(PathsResponse {
        home_dir: SusiDirs::home_dir(),
        config_dir: SusiDirs::config_dir(),
        data_dir: SusiDirs::data_dir(),
        cache_dir: SusiDirs::cache_dir(),
        substrate_home: SusiDirs::substrate_home(),
    })
}

async fn get_ports() -> Json<PortsResponse> {
    Json(PortsResponse {
        gmcp: susi_paths::ports::GMCP,
        gemi: susi_paths::ports::GEMI,
        udp_discovery: susi_paths::ports::UDP_DISCOVERY,
        gmcp_http: susi_paths::ports::GMCP_HTTP,
    })
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = Router::new()
        .route("/paths", get(get_paths))
        .route("/ports", get(get_ports));

    let port = std::env::var("SUSI_PATHS_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("susi-paths service listening on {addr}");
    axum::serve(listener, app).await
}
