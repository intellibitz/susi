//! A real stdio MCP child compiled separately, so libtest never owns its stdout.
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::{Command, Stdio};
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
    let row = format!("{{\"event\":{},\"data\":{}}}\n", quoted(event), data);
    file.write_all(row.as_bytes()).unwrap();
    file.flush().unwrap();
}

fn environment_value(name: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|value| quoted(&value))
        .unwrap_or_else(|| "null".into())
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
    // Separate from the tool result so existing exact-result assertions retain
    // their original schema. All names/values belong only to this fixture.
    trace(
        path,
        "env-observed",
        &format!(
            "{{\"upper\":{},\"lower\":{},\"unicode\":{}}}",
            environment_value("SUSI_ACP_FIXTURE_ALIAS"),
            environment_value("susi_acp_fixture_alias"),
            environment_value("SUSI_ACP_FIXTURE_Å")
        ),
    );
    if mode == "descendant-helper" {
        // Ignore stdin and keep all three inherited pipes open even after the
        // MCP parent exits. Only the host's process containment may stop us.
        let mut heartbeat = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.with_extension("heartbeat"))
            .unwrap();
        loop {
            writeln!(heartbeat, "beat").unwrap();
            heartbeat.flush().unwrap();
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    let mode = if let Some(mode) = mode.strip_suffix("-descendant") {
        let helper_trace = path.with_extension("helper.jsonl");
        // Deliberately leave the helper alive. The host must terminate the
        // whole MCP workload, including when this leader exits normally.
        let _helper = Command::new(std::env::current_exe().unwrap())
            .arg(&helper_trace)
            .arg("descendant-helper")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        while !fs::metadata(helper_trace.with_extension("heartbeat")).is_ok_and(|m| m.len() > 0) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if mode == "parent-exit" {
            return;
        }
        mode
    } else {
        mode
    };
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
            if mode == "tool-error" {
                "{\"content\":[{\"type\":\"text\",\"text\":\"ACP fixture deliberate MCP tool error\"}],\"isError\":true}".to_owned()
            } else {
                format!(
                    "{{\"content\":[{{\"type\":\"text\",\"text\":{}}}],\"isError\":false}}",
                    quoted(&probe)
                )
            }
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
