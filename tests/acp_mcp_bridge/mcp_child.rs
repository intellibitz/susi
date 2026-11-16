//! A real stdio MCP child compiled separately, so libtest never owns its stdout.
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

fn quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn trace(path: &Path, event: &str, data: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{{\"event\":{},\"data\":{}}}", quoted(event), data).unwrap();
    file.flush().unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(&args[1]);
    let mode = args.get(2).map(String::as_str).unwrap_or("normal");
    let cwd = std::env::current_dir().unwrap();
    let literal = std::env::var("SUSI_ACP_FIXTURE_LITERAL").unwrap();
    let probe = format!(
        "{{\"cwd\":{},\"literal\":{},\"pid\":{}}}",
        quoted(&cwd.to_string_lossy()),
        quoted(&literal),
        std::process::id()
    );
    trace(path, "started", &probe);
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        trace(path, "request", &line);
        // McpHost emits integer request IDs. Preserve them instead of assuming
        // a particular initialization/list/call sequence.
        let Some((_, tail)) = line.split_once("\"id\"") else {
            continue;
        };
        let id: String = tail
            .split_once(':')
            .unwrap()
            .1
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        assert!(!id.is_empty(), "fixture expects integer host request IDs");
        let result = if line.contains("\"initialize\"") {
            if mode == "pending-init" {
                trace(path, "blocked", "\"initialize\"");
                loop {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            "{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"acp-bridge-fixture\",\"version\":\"1\"}}".to_owned()
        } else if line.contains("\"tools/list\"") {
            "{\"tools\":[{\"name\":\"fixture_probe\",\"description\":\"ACP bridge fixture session probe\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"marker\":{\"type\":\"string\"}},\"required\":[\"marker\"],\"additionalProperties\":false}}]}".to_owned()
        } else if line.contains("\"tools/call\"") {
            trace(path, "called", &probe);
            if mode == "pending-call" {
                trace(path, "blocked", "\"tools/call\"");
                loop {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            format!(
                "{{\"content\":[{{\"type\":\"text\",\"text\":{}}}],\"isError\":false}}",
                quoted(&probe)
            )
        } else if line.contains("\"ping\"") {
            "{}".to_owned()
        } else {
            panic!("unexpected host request: {line}")
        };
        println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}");
        std::io::stdout().flush().unwrap();
    }
    trace(path, "eof", "null");
}
