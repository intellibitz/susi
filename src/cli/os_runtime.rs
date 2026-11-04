//! `susi os runtime`: the CLI side of the runtime view. The view itself is
//! `susi_daemon::runtime_view` (the MCP `os_runtime` tool returns the same
//! document); this module reads the daemon's inference cache over the GEMI
//! port and renders the table.

/// Ask the daemon's GEMI plane which weights its inference cache holds
/// (`GET /runtime/models`). This is the authoritative "loaded" signal; the
/// `/proc` scan above only sees mapped or open files.
pub(crate) fn daemon_loaded_models() -> Result<serde_json::Value, String> {
    let value = daemon_request("GET", "/runtime/models", None, 3)?;
    if value.get("models").is_some_and(serde_json::Value::is_array) {
        Ok(value)
    } else {
        Err("response has no `models` list".to_string())
    }
}

/// One authenticated loopback call to the daemon's GEMI port. Returns the
/// JSON body on 2xx, else the server's error message (or the status line).
pub(crate) fn daemon_request(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    timeout_secs: u64,
) -> Result<serde_json::Value, String> {
    use std::io::{Read, Write};
    let port = susi_config::SusiConfig::load_global()
        .unwrap_or_default()
        .gemi_port();
    let addr = std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let mut stream = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3))
        .map_err(|e| format!("GEMI unreachable on {addr}: {e}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let auth = susi_paths::host_token()
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let payload = body.map(serde_json::Value::to_string).unwrap_or_default();
    let content = if body.is_some() {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        )
    } else {
        String::new()
    };
    stream
        .write_all(
            format!("{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n{auth}{content}\r\n{payload}")
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    parse_response(&raw)
}

fn parse_response(raw: &str) -> Result<serde_json::Value, String> {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response".to_string())?;
    let status = head.lines().next().unwrap_or_default().trim();
    let value = serde_json::from_str::<serde_json::Value>(body);
    if status.starts_with("HTTP/1.1 2") || status.starts_with("HTTP/1.0 2") {
        return value.map_err(|e| e.to_string());
    }
    Err(value
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| status.to_string()))
}

/// The runtime view, with the daemon's loaded models read over GEMI.
pub(crate) fn view() -> serde_json::Value {
    susi_daemon::runtime_view::view(daemon_loaded_models)
}

pub(crate) fn print(json: bool) -> anyhow::Result<()> {
    let body = view();
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let summary = &body["summary"];
    println!("SUSI OS — runtime");
    println!(
        "daemon:     {}",
        body["daemon_pid"]
            .as_u64()
            .map_or_else(|| "not running".to_string(), |pid| format!("pid {pid}"))
    );
    println!(
        "processes:  {} AI process(es) — {} SUSI-owned, {} external",
        summary["processes"], summary["susi_owned"], summary["external"]
    );
    for p in body["processes"].as_array().into_iter().flatten() {
        println!(
            "  {:>7}  {:<8} {:<13} {:>8}  {}{}",
            p["pid"],
            p["owner"].as_str().unwrap_or_default(),
            p["kind"].as_str().unwrap_or_default(),
            p["gpu_mib"]
                .as_u64()
                .map_or_else(String::new, |mib| format!("{mib} MiB")),
            p["command"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(70)
                .collect::<String>(),
            if p["stale_binary"] == true {
                "  [stale binary]"
            } else {
                ""
            },
        );
    }
    let models = &body["models"];
    println!(
        "models:     {} managed in {}",
        summary["managed_models"],
        models["managed_dir"].as_str().unwrap_or_default()
    );
    for m in models["managed"].as_array().into_iter().flatten() {
        let open_by = m["open_by"].as_array().map_or(0, Vec::len);
        println!(
            "  {}{}{}{}",
            m["name"].as_str().unwrap_or_default(),
            if m["selected"] == true {
                "  [selected]"
            } else {
                ""
            },
            if m["loaded_in_daemon"] == true {
                "  [loaded]"
            } else {
                ""
            },
            if open_by > 0 {
                format!("  [mapped/open by {}]", m["open_by"])
            } else {
                String::new()
            },
        );
    }
    if body["models"]["open_by_observed"] != true {
        println!(
            "  mapped/open residency: Linux-only (/proc maps and fds); not observed on {}",
            std::env::consts::OS
        );
    }
    for m in models["external_in_use"].as_array().into_iter().flatten() {
        println!(
            "  external: {}  [open by {}]",
            m["path"].as_str().unwrap_or_default(),
            m["open_by"]
        );
    }
    let loaded = &models["daemon_loaded"];
    match loaded["models"].as_array() {
        Some(list) => {
            println!(
                "daemon inference cache (pid {}): {} model(s) loaded",
                loaded["pid"],
                list.len()
            );
            for m in list {
                let gpu = match (
                    m["backend"]["gpu_layers"].as_u64(),
                    m["backend"]["total_layers"].as_u64(),
                ) {
                    (Some(on), Some(total)) => format!(", {on}/{total} layers on GPU"),
                    (Some(_) | None, _) => String::new(),
                };
                println!(
                    "  {}  [{} on {}{}, {} in flight]",
                    m["path"].as_str().unwrap_or_default(),
                    m["state"].as_str().unwrap_or_default(),
                    m["device"].as_str().unwrap_or("?"),
                    gpu,
                    m["in_flight_users"].as_u64().unwrap_or(0),
                );
            }
        }
        None => println!(
            "daemon inference cache: unavailable ({})",
            loaded["error"].as_str().unwrap_or("unknown error")
        ),
    }
    if summary["stale_binaries"].as_u64().unwrap_or(0) > 0 {
        println!();
        println!(
            "warning: {} process(es) run a binary that was replaced on disk — restart them to pick up the installed build",
            summary["stale_binaries"]
        );
    }
    println!();
    println!("note: {}", body["note"].as_str().unwrap_or_default());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_surface_body_on_success_and_server_message_on_error() {
        let ok = parse_response(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"pid\":9,\"models\":[]}",
        )
        .unwrap();
        assert_eq!(ok["pid"], 9);
        assert_eq!(
            parse_response("HTTP/1.1 401 Unauthorized\r\n\r\n").unwrap_err(),
            "HTTP/1.1 401 Unauthorized"
        );
        assert_eq!(
            parse_response(
                "HTTP/1.1 400 Bad Request\r\n\r\n{\"error\":{\"message\":\"model 'x' not found\"}}"
            )
            .unwrap_err(),
            "model 'x' not found"
        );
        assert!(parse_response("garbage").is_err());
    }
}
