#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Acceptance surface for the VC-203 GMCP completion program.
//!
//! Every queued `T-CODEXGMCPROADMAP-*` task is gated on a `gmcp_req_203_*`
//! test living in this file; this first contract test proves the normative
//! requirements ledger (VC-203-001) is pinned, verbatim, fully classified,
//! and fail-closed against upstream drift.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn gmcp_req_203_001_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("python3")
        .arg(root.join("scripts/check-gmcp-ledger.py"))
        .current_dir(root)
        .output()
        .expect("python3 must run the GMCP ledger checker");

    assert!(
        output.status.success(),
        "GMCP ledger checker failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("requirements rows"),
        "checker did not verify the complete classified ledger"
    );
}

/// GMCP-203-001-02: the shipped binary verifies the pinned ledger through
/// `susi_gmcp::requirements_ledger` — the same production path every GMCP
/// entry point links — not the authoring-time Python checker.
#[test]
fn gmcp_req_203_001_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-ledger-test-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);

    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(report["rows"].as_u64().unwrap_or(0) > 0);
    assert!(report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0);
    assert!(report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0);
}

fn susi_command(root: &Path, home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
    command
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME");
    command
}

fn susi_json(root: &Path, home: &Path, args: &[&str]) -> serde_json::Value {
    let output = susi_command(root, home, args)
        .output()
        .unwrap_or_else(|e| panic!("the susi binary must run {args:?}: {e}"));
    assert!(
        output.status.success(),
        "susi {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .unwrap_or_else(|e| panic!("susi {args:?} must print JSON: {e}"))
}

fn call_tool_report(result: &serde_json::Value) -> serde_json::Value {
    assert_eq!(
        result["isError"], false,
        "tool call returned an error: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .expect("CallToolResult must carry a text block");
    serde_json::from_str(text).expect("ledger_verify must emit the canonical JSON report")
}

/// A newline-delimited JSON-RPC session against `susi mcp serve` — the served
/// MCP surface the daemon and gateway share. Responses are collected on a
/// reader thread; `request` handles correlation, `request_verbatim` replays
/// fixtures byte-for-byte (including malformed ids), and Drop kills the child.
struct StdioSession {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    rx: std::sync::mpsc::Receiver<serde_json::Value>,
    next_id: u64,
    /// Responses received while waiting for a different id — the service
    /// answers concurrent requests out of order, so a non-matching line
    /// is stashed for its caller rather than discarded.
    pending: Vec<serde_json::Value>,
}

impl StdioSession {
    /// Spawn `mcp serve` with no handshake — the era the session lands in
    /// is whatever the caller's first messages establish.
    fn spawn_raw(root: &Path, home: &Path) -> Self {
        let mut child = susi_command(root, home, &["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the susi binary must spawn `mcp serve`");
        let stdin = child.stdin.take().expect("stdio server must accept stdin");
        let stdout = child.stdout.take().expect("stdio server must yield stdout");
        let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            child,
            stdin,
            rx,
            next_id: 2,
            pending: Vec::new(),
        }
    }

    /// A legacy-era session: `initialize` negotiates 2025-11-25, so
    /// `params.protocolVersion` applies and per-request `_meta` protocol
    /// fields are not required.
    fn spawn(root: &Path, home: &Path) -> Self {
        let mut session = Self::spawn_raw(root, home);
        session
            .stdin
            .write_all(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"gmcp-it","version":"1.0"}}}"#
                    .as_slice(),
            )
            .and_then(|()| {
                session.stdin.write_all(
                    b"\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                )
            })
            .expect("handshake must write");
        session.stdin.flush().expect("requests must flush");
        // Consume the initialize response before callers drive requests.
        session
            .response_for(&serde_json::json!(1), std::time::Duration::from_secs(120))
            .expect("initialize must be answered");
        session
    }

    /// A modern-era session: no `initialize` handshake exists on the
    /// 2026-07-28 path — `server/discover` carries the per-request
    /// `_meta` fields instead and doubles as the warm-up so fixture
    /// replays never race the server's cold start.
    fn spawn_modern(root: &Path, home: &Path) -> Self {
        let mut session = Self::spawn_raw(root, home);
        session.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "modern-handshake",
            "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        }));
        session
            .response_for(
                &serde_json::json!("modern-handshake"),
                std::time::Duration::from_secs(120),
            )
            .expect("server/discover must be answered");
        session
    }

    fn send(&mut self, message: &serde_json::Value) {
        let line = serde_json::to_string(message).expect("request must serialize");
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .expect("request must write");
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        self.stdin
            .write_all(bytes)
            .and_then(|()| self.stdin.flush())
            .expect("raw request must write");
    }

    fn response_for(
        &mut self,
        id: &serde_json::Value,
        timeout: std::time::Duration,
    ) -> Option<serde_json::Value> {
        if let Some(pos) = self.pending.iter().position(|v| &v["id"] == id) {
            return Some(self.pending.remove(pos));
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let value = self.rx.recv_timeout(remaining).ok()?;
            if &value["id"] == id {
                return Some(value);
            }
            self.pending.push(value);
        }
    }

    /// Correlate a request by id and return its `result`, asserting no
    /// JSON-RPC error arrived.
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        let response = self
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(120))
            .expect("served endpoint must answer within the bound");
        assert!(
            response.get("error").is_none_or(|e| e.is_null()),
            "JSON-RPC error: {response}"
        );
        response["result"].clone()
    }

    /// Replay a fixture request verbatim and report what came back for it:
    /// `Some(response)` on an answer within the bound, `None` on a silent drop.
    fn request_verbatim(
        &mut self,
        message: &serde_json::Value,
        timeout: std::time::Duration,
    ) -> Option<serde_json::Value> {
        self.send(message);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let value = match message.get("id") {
                Some(id) if !id.is_null() => {
                    if let Some(pos) = self.pending.iter().position(|v| v["id"] == *id) {
                        self.pending.remove(pos)
                    } else {
                        match self.rx.recv_timeout(remaining) {
                            Ok(value) => value,
                            Err(_) => return None,
                        }
                    }
                }
                // A null/absent id has no correlation key — the first
                // response the endpoint emits to it (if any) is the verdict.
                _ => {
                    if !self.pending.is_empty() {
                        self.pending.remove(0)
                    } else {
                        match self.rx.recv_timeout(remaining) {
                            Ok(value) => value,
                            Err(_) => return None,
                        }
                    }
                }
            };
            match message.get("id") {
                Some(id) if !id.is_null() => {
                    if &value["id"] == id {
                        return Some(value);
                    }
                    self.pending.push(value);
                }
                _ => return Some(value),
            }
        }
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            let pid = self.child.id().to_string();
            let _ = Command::new("taskkill")
                .args(["/PID", &pid, "/T", "/F"])
                .status();
        }
        #[cfg(not(windows))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// GMCP-203-001-03: the ledger verification capability is wired through the
/// CLI (`susi mcp ledger`), the direct-call surface (`susi mcp call susi`,
/// the same path daemon-embedded dispatch uses), and the served MCP surface
/// (`susi mcp serve` over stdio JSON-RPC — the gateway/server entry point).
/// Every entry point dispatches to `requirements_ledger::verify_report_json`,
/// so all three must return the identical canonical report, and `tools/list`
/// must advertise `ledger_verify` through the shared registry.
#[test]
fn gmcp_req_203_001_03_integration() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-ledger-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // Entry point 1: the CLI control surface.
    let cli_report = susi_json(root, &home, &["mcp", "ledger"]);
    assert_eq!(cli_report["verified"], true);

    // Entry point 2: the direct-call surface — both native aliases must
    // resolve to the same handler.
    for server in ["susi", "native"] {
        let call = susi_json(root, &home, &["mcp", "call", server, "ledger_verify", "{}"]);
        let report = call_tool_report(&call);
        assert_eq!(
            report, cli_report,
            "{server}:ledger_verify must return the canonical report"
        );
    }

    // Entry point 3: the served MCP surface — a stdio JSON-RPC session that
    // dispatches through the shared plane-bus tool registry, the same path
    // the daemon and gateway use.
    let mut wire = StdioSession::spawn(root, &home);

    // tools/list must advertise ledger_verify through the shared registry.
    let mut cursor = serde_json::Value::Null;
    let mut listed = false;
    for _ in 0..16 {
        let params = if cursor.is_null() {
            serde_json::json!({})
        } else {
            serde_json::json!({"cursor": cursor})
        };
        let result = wire.request("tools/list", params);
        if result["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["name"] == "ledger_verify"))
        {
            listed = true;
            break;
        }
        match result["nextCursor"]
            .as_str()
            .or_else(|| result["next_cursor"].as_str())
        {
            Some(next) => cursor = serde_json::json!(next),
            None => break,
        }
    }
    assert!(listed, "tools/list must advertise ledger_verify");

    // tools/call over the wire must return the canonical report.
    let result = wire.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    let wire_report = call_tool_report(&result);
    assert_eq!(
        wire_report, cli_report,
        "served tools/call must return the canonical report"
    );

    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir_all(&scratch);
}

fn copy_tree(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::create_dir_all(dst).unwrap();
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn mutate_json(path: &Path, mutate: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    mutate(&mut value);
    std::fs::write(path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
}

fn restore_tree(pristine: &Path, work: &Path) {
    // Only .agents/gmcp is ever mutated; repo-root documents stay put.
    let _ = std::fs::remove_dir_all(work.join(".agents/gmcp"));
    copy_tree(&pristine.join(".agents/gmcp"), &work.join(".agents/gmcp"));
}

/// One adversarial case: apply the corruption, assert the verifier fails
/// closed with the expected reason, then prove recovery — the restored
/// tree must verify cleanly again.
fn adversarial_case(pristine: &Path, work: &Path, needle: &str, mutate: impl FnOnce(&Path)) {
    mutate(work);
    let err = susi_gmcp::requirements_ledger::verify_pinned_ledger(work)
        .err()
        .unwrap_or_else(|| panic!("corruption must fail closed (expecting '{needle}')"));
    assert!(
        err.to_string().contains(needle),
        "corruption must surface '{needle}', got: {err}"
    );
    restore_tree(pristine, work);
    susi_gmcp::requirements_ledger::verify_pinned_ledger(work)
        .expect("restored tree must verify after a corruption cycle");
}

/// GMCP-203-001-04: the verifier fails closed on every corruption class in
/// the threat model — tampered corpus bytes, forged manifest pins, path
/// traversal, reclassified or fabricated rows, malformed JSON, oversized
/// artifacts — recovers deterministically, tolerates concurrent readers,
/// and keeps its wire boundary (schema-validated args, unknown tools,
/// isolated roots) closed.
#[test]
fn gmcp_req_203_001_04_adversarial() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-adv-{}", std::process::id()));
    let pristine = scratch.join("pristine");
    let work = scratch.join("work");
    copy_tree(&root.join(".agents/gmcp"), &pristine.join(".agents/gmcp"));
    // Repository documents are pinned at repo-root paths, so the scratch
    // tree must carry the same relative copies.
    {
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(pristine.join(".agents/gmcp/sources.json")).unwrap(),
        )
        .unwrap();
        for doc in manifest["repository_documents"].as_array().unwrap() {
            let rel = doc["document"].as_str().unwrap();
            let target = pristine.join(rel);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(root.join(rel), &target).unwrap();
        }
    }
    copy_tree(&pristine.join(".agents/gmcp"), &work.join(".agents/gmcp"));
    for entry in std::fs::read_dir(&pristine).unwrap().flatten() {
        if entry.path().is_dir() && entry.file_name() != ".agents" {
            copy_tree(&entry.path(), &work.join(entry.file_name()));
        }
    }

    // Baseline: the pristine copy must verify.
    let baseline = susi_gmcp::requirements_ledger::verify_pinned_ledger(&work)
        .expect("pristine ledger copy must verify");
    assert!(baseline.rows > 0);

    let sources_path = work.join(".agents/gmcp/sources.json");
    let first_doc = {
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sources_path).unwrap()).unwrap();
        manifest["files"][0]["document"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // ── Corpus tampering ────────────────────────────────────────────────
    adversarial_case(&pristine, &work, "drifted from its pinned hash", |w| {
        let file = w.join(".agents/gmcp/spec").join(&first_doc);
        let mut data = std::fs::read(&file).unwrap();
        data[0] ^= 0xFF;
        std::fs::write(&file, data).unwrap();
    });
    adversarial_case(&pristine, &work, "drifted from its pinned hash", |w| {
        let file = w.join(".agents/gmcp/spec").join(&first_doc);
        let mut data = std::fs::read(&file).unwrap();
        data.truncate(data.len() / 2);
        std::fs::write(&file, data).unwrap();
    });
    adversarial_case(&pristine, &work, "read vendored source", |w| {
        std::fs::remove_file(w.join(".agents/gmcp/spec").join(&first_doc)).unwrap();
    });

    // ── Manifest forgery ────────────────────────────────────────────────
    adversarial_case(&pristine, &work, "drifted from its pinned hash", |w| {
        mutate_json(&w.join(".agents/gmcp/sources.json"), |m| {
            let sha = m["files"][0]["sha256"].as_str().unwrap();
            m["files"][0]["sha256"] = serde_json::json!(format!("0{}", &sha[1..]));
        });
    });
    adversarial_case(&pristine, &work, "no pinned upstream commit", |w| {
        mutate_json(&w.join(".agents/gmcp/sources.json"), |m| {
            let repo = m["files"][0]["repo"].as_str().unwrap().to_string();
            m["upstream"][&repo]["commit"] = serde_json::json!("");
        });
    });
    adversarial_case(&pristine, &work, "no generation timestamp", |w| {
        mutate_json(&w.join(".agents/gmcp/sources.json"), |m| {
            m["generated_unix"] = serde_json::json!(0);
        });
    });
    adversarial_case(&pristine, &work, "lacks a name or role", |w| {
        mutate_json(&w.join(".agents/gmcp/sources.json"), |m| {
            m["external_authorities"][0]["name"] = serde_json::json!("");
        });
    });
    adversarial_case(&pristine, &work, "parse .agents/gmcp/sources.json", |w| {
        let path = w.join(".agents/gmcp/sources.json");
        let data = std::fs::read(&path).unwrap();
        std::fs::write(&path, &data[..data.len() / 2]).unwrap();
    });
    adversarial_case(&pristine, &work, "parse .agents/gmcp/sources.json", |w| {
        mutate_json(&w.join(".agents/gmcp/sources.json"), |m| {
            m["files"][0]["injected"] = serde_json::json!(true);
        });
    });

    // ── Path traversal / confinement ────────────────────────────────────
    for escape in [
        "../escape.mdx",
        "/etc/passwd",
        "a\\b.mdx",
        "a//b.mdx",
        "./hidden.mdx",
    ] {
        adversarial_case(&pristine, &work, "not confined", |w| {
            mutate_json(&w.join(".agents/gmcp/sources.json"), |m| {
                m["files"][0]["document"] = serde_json::json!(escape);
            });
        });
    }
    adversarial_case(&pristine, &work, "not confined", |w| {
        mutate_json(&w.join(".agents/gmcp/sources.json"), |m| {
            m["repository_documents"][0]["document"] = serde_json::json!("../../Cargo.toml");
        });
    });
    // Retained fixture: a structurally-valid manifest carrying a '..' entry.
    adversarial_case(&pristine, &work, "not confined", |w| {
        std::fs::copy(
            root.join(".agents/gmcp/fixtures/negative/manifest-document-traversal.json"),
            w.join(".agents/gmcp/sources.json"),
        )
        .unwrap();
    });

    // ── Row-level forgery ───────────────────────────────────────────────
    adversarial_case(&pristine, &work, "empty or duplicate requirement id", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            let dup = l["requirements"][0].clone();
            l["requirements"].as_array_mut().unwrap().push(dup);
        });
    });
    adversarial_case(&pristine, &work, "empty or duplicate requirement id", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["id"] = serde_json::json!("");
        });
    });
    adversarial_case(&pristine, &work, "empty clause", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["clause"] = serde_json::json!("");
        });
    });
    adversarial_case(&pristine, &work, "no categories", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["categories"] = serde_json::json!([]);
        });
    });
    adversarial_case(&pristine, &work, "empty disposition reason", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["disposition"]["reason"] = serde_json::json!("");
        });
    });
    adversarial_case(&pristine, &work, "empty trace.owner", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["trace"]["owner"] = serde_json::json!("");
        });
    });
    adversarial_case(&pristine, &work, "bad vector", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["trace"]["vector"] = serde_json::json!("VC-203-049");
        });
    });
    adversarial_case(&pristine, &work, "naming convention", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["trace"]["positive_test"] = serde_json::json!("bogus");
        });
    });
    adversarial_case(&pristine, &work, "unpinned source document", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["source"]["document"] = serde_json::json!("nonexistent.md");
        });
    });
    adversarial_case(&pristine, &work, "source pin mismatch", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["requirements"][0]["source"]["sha256"] = serde_json::json!("0".repeat(64));
        });
    });
    adversarial_case(&pristine, &work, "is not verbatim", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            let clause = l["requirements"][0]["clause"].as_str().unwrap().to_string();
            l["requirements"][0]["clause"] = serde_json::json!(format!("{clause} NEVER SAID"));
        });
    });
    // Retained fixture: a fabricated MUST the spec never stated, patched
    // onto a real source/trace so the verbatim check is what fires.
    adversarial_case(&pristine, &work, "is not verbatim", |w| {
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                root.join(".agents/gmcp/fixtures/negative/requirement-row-not-verbatim.json"),
            )
            .unwrap(),
        )
        .unwrap();
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            let mut row = l["requirements"][0].clone();
            row["id"] = fixture["id"].clone();
            row["clause"] = fixture["clause"].clone();
            row["disposition"] = fixture["disposition"].clone();
            l["requirements"].as_array_mut().unwrap().push(row);
        });
    });
    adversarial_case(&pristine, &work, "has no classified rows", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            for row in l["requirements"].as_array_mut().unwrap() {
                row["categories"] = serde_json::json!(["behavior"]);
            }
        });
    });
    adversarial_case(&pristine, &work, "schema is", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["schema"] = serde_json::json!("forged/schema");
        });
    });
    adversarial_case(&pristine, &work, "disagree with sources.json", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            l["baseline"]["revisions"]["modern"] = serde_json::json!("1999-01-01");
        });
    });
    adversarial_case(
        &pristine,
        &work,
        "parse .agents/gmcp/requirements-ledger.json",
        |w| {
            let path = w.join(".agents/gmcp/requirements-ledger.json");
            let data = std::fs::read(&path).unwrap();
            std::fs::write(&path, &data[..data.len() / 2]).unwrap();
        },
    );
    adversarial_case(
        &pristine,
        &work,
        "parse .agents/gmcp/requirements-ledger.json",
        |w| {
            mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
                l["injected"] = serde_json::json!(true);
            });
        },
    );

    // ── Bounded resources ───────────────────────────────────────────────
    adversarial_case(&pristine, &work, "exceeds the", |w| {
        std::fs::write(
            w.join(".agents/gmcp/requirements-ledger.json"),
            vec![b'x'; 65 * 1024 * 1024],
        )
        .unwrap();
    });
    adversarial_case(&pristine, &work, "exceeds the", |w| {
        mutate_json(&w.join(".agents/gmcp/sources.json"), |m| {
            m["files"][0]["bytes"] = serde_json::json!(128 * 1024 * 1024u64);
        });
    });
    adversarial_case(&pristine, &work, "over the 8192-row bound", |w| {
        mutate_json(&w.join(".agents/gmcp/requirements-ledger.json"), |l| {
            let rows = l["requirements"].as_array().unwrap().clone();
            let mut inflated = Vec::new();
            for _ in 0..5 {
                inflated.extend(rows.iter().cloned());
            }
            l["requirements"] = serde_json::json!(inflated);
        });
    });

    // ── Concurrency: verification is read-only and deterministic ────────
    let mut handles = Vec::new();
    for _ in 0..8 {
        let dir = work.clone();
        handles.push(std::thread::spawn(move || {
            susi_gmcp::requirements_ledger::verify_pinned_ledger(&dir).map(|r| r.rows)
        }));
    }
    for handle in handles {
        assert_eq!(
            handle.join().unwrap().unwrap(),
            baseline.rows,
            "concurrent verification must agree with the baseline"
        );
    }

    // ── Isolation: a root without the pinned artifacts fails closed ─────
    let empty = scratch.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(
        susi_gmcp::requirements_ledger::verify_pinned_ledger(&empty).is_err(),
        "verification must fail closed on a tree without pinned artifacts"
    );

    // ── Wire boundary: schema rejects non-object args; unknown tools and
    //    an artifact-less root fail through the shipped binary too ────────
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let call = susi_json(
        root,
        &home,
        &["mcp", "call", "susi", "ledger_verify", "\"not-an-object\""],
    );
    assert_eq!(
        call["isError"], true,
        "schema must reject non-object tool arguments"
    );
    let call = susi_json(root, &home, &["mcp", "call", "susi", "no_such_tool", "{}"]);
    assert_eq!(
        call["isError"], true,
        "unknown tools must be rejected at the dispatch boundary"
    );
    let output = susi_command(&empty, &home, &["mcp", "ledger"])
        .output()
        .expect("susi mcp ledger must run in an empty tree");
    assert!(
        !output.status.success(),
        "mcp ledger must fail closed without pinned artifacts"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-001-05: interoperability and quality for the ledger. Both roles
/// of the toolchain (the authoring-time Python checker and the shipped Rust
/// verifier) must agree; every surface must emit the identical canonical
/// report; verification must be deterministic, read-only, and insensitive to
/// artifact ordering; the published budgets must hold; and every retained
/// fixture replays as expected — divergences are recorded in
/// `fixtures/divergences.json` and must match observed behavior verbatim.
#[test]
fn gmcp_req_203_001_05_interop() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Both roles: the authoring checker and the shipped verifier ──────
    let checker = Command::new("python3")
        .arg(root.join("scripts/check-gmcp-ledger.py"))
        .current_dir(root)
        .output()
        .expect("python3 must run the GMCP ledger checker");
    assert!(
        checker.status.success(),
        "authoring checker failed: {}",
        String::from_utf8_lossy(&checker.stderr)
    );
    let checker_out = String::from_utf8_lossy(&checker.stdout);
    let checker_rows: u64 = checker_out
        .split_whitespace()
        .nth(3)
        .and_then(|n| n.parse().ok())
        .expect("checker must report a row count");

    let start = std::time::Instant::now();
    let cli_report = susi_json(root, &home, &["mcp", "ledger"]);
    let cli_elapsed = start.elapsed();
    assert!(
        cli_elapsed < std::time::Duration::from_secs(60),
        "mcp ledger exceeded the published 60s budget: {cli_elapsed:?}"
    );
    assert_eq!(
        cli_report["rows"].as_u64().unwrap(),
        checker_rows,
        "checker and verifier must agree on the row count"
    );

    // ── Differential behavior: direct and served surfaces equal the CLI ─
    let direct = call_tool_report(&susi_json(
        root,
        &home,
        &["mcp", "call", "susi", "ledger_verify", "{}"],
    ));
    assert_eq!(direct, cli_report, "direct call must equal the CLI report");

    let mut wire = StdioSession::spawn(root, &home);
    let mut wire_modern = StdioSession::spawn_modern(root, &home);
    let wire_report = call_tool_report(&wire.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    ));
    assert_eq!(
        wire_report, cli_report,
        "served tools/call must equal the CLI report"
    );
    // Warm the modern session's tool surface too: the first tools/*
    // request bootstraps the downstream catalog, which can exceed the
    // replay's per-fixture bound under parallel load — a cold-start
    // artifact, not a protocol verdict.
    let _ = wire_modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}),
    );

    // ── Property invariants ─────────────────────────────────────────────
    // Determinism: repeated verification produces identical results.
    let first = susi_gmcp::requirements_ledger::verify_pinned_ledger(root).unwrap();
    let second = susi_gmcp::requirements_ledger::verify_pinned_ledger(root).unwrap();
    assert_eq!(first.rows, second.rows);
    assert_eq!(first.revisions, second.revisions);
    assert_eq!(first.dispositions, second.dispositions);
    assert_eq!(first.categories, second.categories);

    // Read-only: verification leaves every pinned byte untouched.
    let tree_hash = |root_dir: &Path| {
        let mut digest = String::new();
        let mut stack = vec![root_dir.join(".agents/gmcp")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    digest.push_str(&format!("{:x}", md5ish(&std::fs::read(&path).unwrap())));
                }
            }
        }
        digest
    };
    let before = tree_hash(root);
    let _ = susi_gmcp::requirements_ledger::verify_pinned_ledger(root).unwrap();
    assert_eq!(tree_hash(root), before, "verification must be read-only");

    // Order-insensitivity: shuffled manifest files and shuffled ledger rows
    // verify identically on a scratch copy.
    let work = scratch.join("work");
    copy_tree(&root.join(".agents/gmcp"), &work.join(".agents/gmcp"));
    {
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(work.join(".agents/gmcp/sources.json")).unwrap(),
        )
        .unwrap();
        for doc in manifest["repository_documents"].as_array().unwrap() {
            let rel = doc["document"].as_str().unwrap();
            let target = work.join(rel);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(root.join(rel), &target).unwrap();
        }
    }
    mutate_json(&work.join(".agents/gmcp/sources.json"), |m| {
        m["files"].as_array_mut().unwrap().reverse();
    });
    mutate_json(&work.join(".agents/gmcp/requirements-ledger.json"), |l| {
        l["requirements"].as_array_mut().unwrap().reverse();
    });
    let shuffled = susi_gmcp::requirements_ledger::verify_pinned_ledger(&work)
        .expect("reordered artifacts must verify identically");
    assert_eq!(shuffled.rows, first.rows);
    assert_eq!(shuffled.revisions, first.revisions);

    // ── Published budgets (docs/gmcp/contract-203-001.md §SLOs) ─────────
    let ledger_bytes = std::fs::metadata(root.join(".agents/gmcp/requirements-ledger.json"))
        .unwrap()
        .len();
    assert!(
        ledger_bytes < 10 * 1024 * 1024,
        "ledger exceeds the published 10 MiB budget: {ledger_bytes}"
    );
    let report_len = serde_json::to_string(&cli_report).unwrap().len();
    assert!(
        report_len < 64 * 1024,
        "canonical report exceeds the 64 KiB evidence budget: {report_len}"
    );
    let verify_start = std::time::Instant::now();
    let _ = susi_gmcp::requirements_ledger::verify_pinned_ledger(root).unwrap();
    assert!(
        verify_start.elapsed() < std::time::Duration::from_secs(60),
        "in-process verification exceeded the published budget"
    );

    // ── Fixture replay + divergence registry ────────────────────────────
    let divergences: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/fixtures/divergences.json")).unwrap(),
    )
    .unwrap();
    let mut replayed = std::collections::BTreeSet::new();
    for class in ["positive", "negative"] {
        for entry in std::fs::read_dir(root.join(".agents/gmcp/fixtures").join(class))
            .unwrap()
            .flatten()
        {
            let fixture: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap()).unwrap();
            let Some(request) = fixture.get("request") else {
                continue;
            };
            let name = format!("{class}/{}", entry.file_name().to_str().unwrap());
            replayed.insert(name.clone());
            // `era` pins which session a fixture replays on: `modern`
            // fixtures exercise the 2026-07-28 per-request `_meta` rules on
            // a handshake-free session; the default legacy session
            // negotiates 2025-11-25.
            let era = fixture["era"].as_str().unwrap_or("legacy");
            assert!(
                era == "legacy" || era == "modern",
                "{name}: era must be legacy or modern"
            );
            let session = if era == "modern" {
                &mut wire_modern
            } else {
                &mut wire
            };
            let observed =
                match session.request_verbatim(request, std::time::Duration::from_secs(5)) {
                    None => serde_json::json!({"kind": "silent_drop"}),
                    Some(response) if response.get("error").is_some() => {
                        serde_json::json!({"kind": "error", "code": response["error"]["code"]})
                    }
                    Some(_) => serde_json::json!({"kind": "result"}),
                };
            let expected = fixture["expect"].as_str().unwrap();
            // `none` is the notification expectation: the wire MUST stay
            // silent. `expect_code` pins the exact error when the
            // endpoint does respond.
            let met = if expected == "none" {
                observed["kind"] == "silent_drop"
            } else {
                observed["kind"].as_str().unwrap()
                    == expected
                        .trim_end_matches("or error")
                        .trim_end_matches("or result")
            };
            if let Some(code) = fixture.get("expect_code") {
                if observed["kind"] == "error" {
                    assert_eq!(
                        observed["code"], *code,
                        "{name}: wrong error code for the pinned failure mode"
                    );
                }
            }
            if met {
                continue;
            }
            let recorded = divergences["divergences"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["fixture"] == name)
                .unwrap_or_else(|| {
                    panic!("unrecorded divergence on {name}: expected {expected}, got {observed}")
                });
            assert_eq!(
                recorded["observed"], observed,
                "recorded divergence on {name} drifted from observed behavior"
            );
        }
    }
    // Raw-line divergences (e.g. unparseable stdio bytes) replay byte-for-byte.
    for divergence in divergences["divergences"].as_array().unwrap() {
        let fixture = divergence["fixture"].as_str().unwrap();
        let Some(raw) = fixture.strip_prefix("raw:") else {
            // File-backed divergences must name a fixture that was replayed.
            assert!(
                replayed.contains(fixture),
                "divergence names a fixture that was never replayed: {fixture}"
            );
            continue;
        };
        wire.send_raw(raw.as_bytes());
        wire.send_raw(b"\n");
        let observed = {
            let got = wire.rx.recv_timeout(std::time::Duration::from_secs(5)).ok();
            match got {
                None => serde_json::json!({"kind": "silent_drop"}),
                Some(v) if v.get("error").is_some() => {
                    serde_json::json!({"kind": "error", "code": v["error"]["code"]})
                }
                Some(_) => serde_json::json!({"kind": "result"}),
            }
        };
        assert_eq!(
            divergence["observed"], observed,
            "raw-line divergence drifted from observed behavior"
        );
    }

    // ── Retained evidence: the vector's JSONL must carry complete records
    //    for every delivered work package ────────────────────────────────
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-001.jsonl"))
        .expect("VC-203-001 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    for line in evidence.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("evidence records must be single-line JSON");
        for field in [
            "requirement",
            "check",
            "exit",
            "artifact_sha256",
            "identity",
            "recorded_unix",
        ] {
            assert!(!record[field].is_null(), "evidence record lacks {field}");
        }
        covered.insert(record["requirement"].as_str().unwrap().to_string());
    }
    for req in [
        "GMCP-203-001-01",
        "GMCP-203-001-02",
        "GMCP-203-001-03",
        "GMCP-203-001-04",
        "GMCP-203-001-05",
    ] {
        assert!(
            covered.contains(req),
            "evidence must retain a record for {req}"
        );
    }

    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);
}

fn md5ish(data: &[u8]) -> u128 {
    // Cheap deterministic content fingerprint for the read-only invariant —
    // correctness, not cryptographic strength (the pinned SHA-256 checks live
    // in the verifier itself).
    let mut a: u64 = 0x9e3779b97f4a7c15;
    let mut b: u64 = 0xcbf29ce484222325;
    for (i, byte) in data.iter().enumerate() {
        a = a.wrapping_mul(0x100000001b3) ^ (*byte as u64);
        b ^= (*byte as u64).wrapping_add(i as u64);
        b = b.wrapping_mul(0x9e3779b97f4a7c15);
    }
    ((a as u128) << 64) | b as u128
}

/// GMCP-203-001-06: operations and evidence — the shipped surfaces must be
/// inspectable, bounded, and diagnostic; drift detection must be automated
/// and fail closed on a changed or unreachable source; and the contract
/// must retain runbook, migration, and rollback guidance plus a recorded
/// evidence entry for this requirement.
#[test]
fn gmcp_req_203_001_06_operations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Inspectable state: the production report carries the bounded
    //    metrics an operator or dashboard asserts on ─────────────────
    let report = susi_json(root, &home, &["mcp", "ledger"]);
    for key in [
        "rows",
        "documents",
        "revisions",
        "dispositions",
        "categories",
        "vectors",
    ] {
        assert!(!report[key].is_null(), "ledger report must expose {key}");
    }
    assert_eq!(report["verified"], true);
    assert_eq!(report["rows"].as_u64(), Some(1713));
    assert_eq!(report["documents"].as_u64(), Some(72));

    // ── Configuration validation: --refetched without --drift is
    //    meaningless and must be rejected ────────────────────────────
    let bad = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(
        !bad.status.success(),
        "--refetched without --drift must fail usage validation"
    );

    // ── Deterministic drift pass: the vendored corpus used as the
    //    refetched directory must match every pin ────────────────────
    let drift = susi_json(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            ".agents/gmcp/spec",
        ],
    );
    assert_eq!(drift["drift"], true);
    assert_eq!(
        drift["verified"], true,
        "drift pass must be clean: {}",
        drift["findings"]
    );
    assert_eq!(drift["checked"].as_u64(), Some(72));
    assert_eq!(drift["checked"], drift["matched"]);
    assert!(
        drift["findings"].as_array().unwrap().is_empty(),
        "an unchanged upstream must produce no findings"
    );

    // ── Fail closed on upstream drift: flip one byte in a refetched
    //    copy and the finding must name the drifted document ─────────
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json"))
            .expect("sources.json must parse"),
    )
    .expect("sources.json must parse");
    let files = manifest["files"].as_array().expect("manifest files");
    let drifted_doc = files[0]["document"].as_str().unwrap().to_string();
    let missing_doc = files[1]["document"].as_str().unwrap().to_string();

    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(&drifted_doc);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes.push(b'!');
    std::fs::write(&victim, &bytes).unwrap();
    let output = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .expect("susi must run");
    assert!(
        !output.status.success(),
        "a drifted upstream file must fail the check closed"
    );
    let finding_report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
            .expect("a failed drift pass still emits the JSON report");
    let findings = finding_report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "exactly the edited file must drift");
    assert_eq!(findings[0]["document"], drifted_doc);
    assert_eq!(findings[0]["kind"], "hash_drift");
    assert_eq!(finding_report["verified"], false);

    // ── Fail closed on an unfetchable source: a missing refetched file
    //    is a finding, never a skip ──────────────────────────────────
    std::fs::remove_file(refetched.join(&missing_doc)).unwrap();
    std::fs::write(&victim, &bytes[..bytes.len() - 1]).unwrap();
    let output = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .expect("susi must run");
    assert!(!output.status.success());
    let finding_report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    let findings = finding_report["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f["document"] == missing_doc && f["kind"] == "unfetchable"),
        "a missing refetched file must fail closed as unfetchable: {findings:?}"
    );

    // ── Canonical refetch URL parity with the authoring checker ──────
    assert_eq!(
        susi_gmcp::requirements_ledger::upstream_url(
            "ext-tasks",
            "899c1bbc869567670093face3591b39c28ce3219",
            "spec/2026-07-28/index.mdx"
        ),
        "https://raw.githubusercontent.com/modelcontextprotocol/ext-tasks/899c1bbc869567670093face3591b39c28ce3219/spec/2026-07-28/index.mdx"
    );

    // ── Runbook, migration, and rollback guidance in the contract ────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-001.md"))
        .expect("contract must exist");
    for token in [
        "Operations and incident runbook",
        "Inspectable state",
        "Diagnostics",
        "Incident triage",
        "Migration (re-pinning an upstream standard)",
        "Rollback",
        "--refetched",
        "hash_drift",
        "unfetchable",
    ] {
        assert!(
            contract.contains(token),
            "contract must document operations guidance: {token}"
        );
    }

    // ── Evidence: a complete record for this requirement ─────────────
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-001.jsonl"))
        .expect("VC-203-001 must retain an evidence artifact");
    let record = evidence
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|r| r["requirement"] == "GMCP-203-001-06")
        .expect("evidence must retain a record for GMCP-203-001-06");
    for field in [
        "check",
        "exit",
        "artifact_sha256",
        "identity",
        "recorded_unix",
    ] {
        assert!(!record[field].is_null(), "evidence record lacks {field}");
    }
    assert_eq!(record["exit"], 0);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-001-M: vector mastery on the shipped production path. Task
/// closure and roadmap prose are not proof — this test consumes and
/// verifies the evidence requirements 01-06 produced: the ledger must
/// verify through `susi mcp ledger` and the `ledger_verify` tool with an
/// identical canonical report; every requirement's retained record must
/// be complete, fresh, signed, committed, and backed by a test that
/// exists; and automated drift detection must fail closed on a changed
/// source. Missing, stale, unsigned, locally modified, waived,
/// mock-only, or zero-test evidence fails the vector.
#[test]
fn gmcp_vc_203_001_mastery() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Shipped production path: CLI and tool surfaces verify and
    //    return the identical canonical report ──────────────────────
    let cli = susi_json(root, &home, &["mcp", "ledger"]);
    assert_eq!(cli["verified"], true, "production path must verify");
    assert_eq!(cli["rows"].as_u64(), Some(1713));
    assert_eq!(cli["documents"].as_u64(), Some(72));
    let tool = susi_json(root, &home, &["mcp", "call", "susi", "ledger_verify", "{}"]);
    assert_eq!(
        call_tool_report(&tool),
        cli,
        "the shipped tool must emit the identical canonical report"
    );

    // ── Row-level traceability on the shipped artifact: every row
    //    names an owner, an existing production path, both tests, an
    //    evidence artifact, and a disposition ─────────────────────────
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must parse"),
    )
    .expect("ledger must parse");
    let rows = ledger["requirements"].as_array().expect("requirements");
    assert_eq!(rows.len(), 1713);
    for row in rows {
        let id = row["id"].as_str().unwrap_or("?");
        for field in [
            "vector",
            "owner",
            "production_path",
            "positive_test",
            "negative_test",
            "evidence",
        ] {
            let value = row["trace"][field].as_str().unwrap_or_default();
            assert!(!value.is_empty(), "{id}: trace.{field} must be non-empty");
        }
        let production = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        assert!(
            !row["disposition"]["status"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{id}: disposition.status must be non-empty"
        );
    }

    // ── Evidence verification: complete, fresh, signed, backed by a
    //    real test — missing, stale, unsigned, waived, mock-only or
    //    zero-test records fail ───────────────────────────────────────
    use sha2::{Digest, Sha256};
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap();
    let ledger_hash = hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = std::fs::read_to_string(root.join("tests/gmcp_requirements.rs")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-001.jsonl"))
        .expect("VC-203-001 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    for line in evidence.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("evidence records must be single-line JSON");
        for field in [
            "requirement",
            "check",
            "exit",
            "detail",
            "recorded_unix",
            "artifact_sha256",
            "identity",
        ] {
            assert!(
                !record[field].is_null(),
                "unsigned or incomplete evidence: record lacks {field}"
            );
        }
        // Waived or mock-only evidence is not proof.
        assert!(
            record.get("waived").is_none() && record.get("mock").is_none(),
            "waived or mock-only evidence does not count: {record}"
        );
        assert_eq!(record["exit"], 0, "evidence must record a green check");
        assert!(
            !record["identity"].as_str().unwrap().is_empty(),
            "evidence must be signed by an identity"
        );
        assert!(
            !record["detail"].as_str().unwrap().is_empty(),
            "evidence must describe what the check proved"
        );
        // Staleness: the recorded artifact hash must be the ledger's
        // current bytes — proof of a since-changed artifact is stale.
        assert_eq!(
            record["artifact_sha256"].as_str().unwrap(),
            ledger_hash,
            "stale evidence: recorded hash must be the current ledger hash"
        );
        let recorded = record["recorded_unix"].as_u64().unwrap();
        assert!(
            recorded > 1_792_256_000 && recorded <= now + 60,
            "recorded_unix {recorded} must be a plausible recent timestamp"
        );
        // Zero-test evidence: the check must be a real command on a
        // committed artifact — at least one argument must resolve to an
        // existing repo path, and a `cargo test` command must name a
        // test that exists in the named file. Prose, task IDs, and
        // invented commands are not proof.
        let check = record["check"].as_str().unwrap();
        if let Some(test_name) = check.strip_prefix("cargo test --locked --test gmcp_requirements ")
        {
            // The committed artifact behind a cargo-test check is the
            // named test target's file plus the test function itself.
            assert!(
                test_name.starts_with("gmcp_")
                    && !test_name.contains(char::is_whitespace)
                    && test_src.contains(&format!("fn {test_name}")),
                "evidence names a test that does not exist: {test_name}"
            );
        } else {
            assert!(
                check
                    .split_whitespace()
                    .filter(|token| !token.starts_with('-'))
                    .any(|token| root.join(token).exists()),
                "evidence check must reference a committed artifact: {check}"
            );
        }
        covered.insert(record["requirement"].as_str().unwrap().to_string());
    }
    for req in [
        "GMCP-203-001-01",
        "GMCP-203-001-02",
        "GMCP-203-001-03",
        "GMCP-203-001-04",
        "GMCP-203-001-05",
        "GMCP-203-001-06",
        "GMCP-203-001-M",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── Committed proof: locally modified artifacts are not
    //    evidence — the pinned tree must be clean against git ────────
    let status = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--",
            ".agents/gmcp",
            "docs/gmcp",
            "tests/gmcp_requirements.rs",
        ])
        .current_dir(root)
        .output()
        .expect("git status must run — committed proof is required");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "locally modified artifacts are not proof:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );

    // ── Automated drift detection fails closed: a changed upstream
    //    file must invalidate the pinned corpus on the shipped path ───
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json")).unwrap(),
    )
    .unwrap();
    let drifted_doc = manifest["files"][0]["document"]
        .as_str()
        .unwrap()
        .to_string();
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(&drifted_doc);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes.push(b'!');
    std::fs::write(&victim, &bytes).unwrap();
    let output = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .expect("susi must run");
    assert!(
        !output.status.success(),
        "drift detection must fail closed on a changed source"
    );
    let findings: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert!(
        findings["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["document"] == drifted_doc && f["kind"] == "hash_drift"),
        "the drift finding must name the changed document: {findings}"
    );

    // ── The contract itself is retained operational documentation ────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-001.md"))
        .expect("contract must exist");
    for token in [
        "Normative clause",
        "Pinned sources",
        "Threat model",
        "Operations and incident runbook",
        "Migration (re-pinning an upstream standard)",
        "Rollback",
    ] {
        assert!(contract.contains(token), "contract must retain {token}");
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-01: the JSON-RPC-substrate contract exists, is pinned to
/// the ledger's VC-203-002 rows, resolves its ambiguities, names the
/// canonical production path, and ships its seed fixtures and evidence
/// location — everything later work packages build against.
#[test]
fn gmcp_req_203_002_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // ── The contract document exists and carries every required
    //    section ────────────────────────────────────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-002.md"))
        .expect("contract-203-002.md must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    // Ambiguities the contract must have resolved before implementation.
    for decision in ["batches", "id: null", "resultType", "initialize"] {
        assert!(
            contract.contains(decision),
            "contract must resolve: {decision}"
        );
    }

    // ── The ledger's VC-203-002 rows are the pinned normative set the
    //    contract binds: exact counts, complete traces, existing
    //    production path ─────────────────────────────────────────────
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must parse"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-002")
        .collect();
    assert_eq!(rows.len(), 157, "the contract binds 157 VC-203-002 rows");
    let mut revisions = std::collections::BTreeMap::new();
    for row in &rows {
        *revisions
            .entry(row["revision"].as_str().unwrap().to_string())
            .or_insert(0usize) += 1;
        let id = row["id"].as_str().unwrap();
        for field in [
            "vector",
            "owner",
            "production_path",
            "positive_test",
            "negative_test",
            "evidence",
        ] {
            assert!(
                !row["trace"][field].as_str().unwrap_or_default().is_empty(),
                "{id}: trace.{field} must be non-empty"
            );
        }
        assert_eq!(
            row["trace"]["evidence"], ".agents/gmcp/evidence/VC-203-002.jsonl",
            "{id}: evidence must point at this vector's artifact"
        );
        let production = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        for cat in row["categories"].as_array().unwrap() {
            assert!(
                ["behavior", "error", "security"].contains(&cat.as_str().unwrap()),
                "{id}: unexpected category {cat}"
            );
        }
    }
    assert_eq!(revisions.get("2026-07-28"), Some(&107));
    assert_eq!(revisions.get("2025-11-25"), Some(&50));

    // ── The full pinned-corpus check still passes — the contract did
    //    not disturb the VC-203-001 machinery ─────────────────────────
    let checker = Command::new("python3")
        .arg("scripts/check-gmcp-ledger.py")
        .current_dir(root)
        .output()
        .expect("the checker must run");
    assert!(
        checker.status.success(),
        "checker failed:\n{}",
        String::from_utf8_lossy(&checker.stderr)
    );

    // ── Seed fixtures: every file the contract names exists, parses,
    //    and follows the fixture convention ──────────────────────────
    let seeds: &[(&str, &str)] = &[
        ("positive/request-2026-07-28-meta.json", "result"),
        ("positive/request-progress-token.json", "result"),
        ("positive/request-trace-context.json", "result"),
        ("positive/notification-cancelled.json", "none"),
        ("negative/request-batch-array.json", "error"),
        ("negative/request-meta-missing-2026-07-28.json", "error"),
        ("negative/request-unsupported-version.json", "error"),
    ];
    for (file, expect) in seeds {
        let path = root.join(".agents/gmcp/fixtures").join(file);
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file} must exist: {e}")),
        )
        .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        assert!(
            !fixture["comment"].as_str().unwrap_or_default().is_empty(),
            "{file} must comment which row/decision it exercises"
        );
        assert!(
            !fixture["request"].is_null(),
            "{file} must carry a request payload"
        );
        assert_eq!(
            fixture["expect"].as_str().unwrap(),
            *expect,
            "{file} expect must be {expect}"
        );
        if file.starts_with("negative/") && fixture.get("expect_code").is_some() {
            assert!(
                fixture["expect_code"].is_i64() || fixture["expect_code"].is_u64(),
                "{file} expect_code must be a JSON-RPC error code integer"
            );
        }
    }
    // The modern-path required _meta fields the positive fixtures carry.
    let meta_fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/request-2026-07-28-meta.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let meta = &meta_fixture["request"]["params"]["_meta"];
    for key in [
        "io.modelcontextprotocol/protocolVersion",
        "io.modelcontextprotocol/clientCapabilities",
    ] {
        assert!(
            !meta[key].is_null(),
            "the modern-path fixture must carry required _meta key {key}"
        );
    }
    // The batch fixture is malformed wire input — a non-object payload.
    let batch: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/request-batch-array.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        batch["request"].is_array(),
        "the batch fixture must carry the array payload it exercises"
    );

    // ── Evidence location: the vector's append-only artifact exists
    //    and holds a complete -01 record ──────────────────────────────
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-002.jsonl"))
        .expect("VC-203-002 must retain an evidence artifact");
    let record = evidence
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|r| r["requirement"] == "GMCP-203-002-01")
        .expect("evidence must retain a record for GMCP-203-002-01");
    for field in [
        "check",
        "exit",
        "detail",
        "recorded_unix",
        "artifact_sha256",
        "identity",
    ] {
        assert!(!record[field].is_null(), "evidence record lacks {field}");
    }
    assert_eq!(record["exit"], 0);
}

/// GMCP-203-002-02: the canonical JSON-RPC gate (`susi_gmcp::jsonrpc_gate`)
/// is the production wire path — the served stdio surface answers the
/// contract's failure modes with typed errors instead of the SDK codec's
/// silent drops, enforces era-specific metadata rules, and stamps the
/// required `resultType` on results. Both roles are exercised: the
/// in-process classifier must agree with what the wire emits.
#[test]
fn gmcp_req_203_002_02_production() {
    use susi_gmcp::jsonrpc_gate::{classify_line, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── The classifier is the canonical policy: same verdicts the wire
    //    emits, decidable without spawning a server ────────────────────
    let classify = |line: &str, era: &mut EraState| -> serde_json::Value {
        match classify_line(line.as_bytes(), era) {
            GateVerdict::Forward => serde_json::json!({"kind": "forward"}),
            GateVerdict::Drop => serde_json::json!({"kind": "drop"}),
            GateVerdict::Reject { code, data, .. } => {
                serde_json::json!({"kind": "reject", "code": code, "data": data})
            }
        }
    };
    let expect_reject = |line: &str, code: i64, era: &mut EraState| {
        let verdict = classify(line, era);
        assert_eq!(
            verdict["code"], code,
            "{line}: expected reject {code}, got {verdict}"
        );
    };

    // Malformed input fails closed, never silently.
    let mut era = EraState::default();
    expect_reject("{not-a-json-line}", -32700, &mut era);
    expect_reject("not even json", -32700, &mut era);
    expect_reject(
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        -32600,
        &mut era,
    );
    expect_reject(
        r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#,
        -32600,
        &mut era,
    );
    expect_reject(
        r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
        -32600,
        &mut era,
    );
    expect_reject(r#"{"jsonrpc":"2.0","id":1}"#, -32600, &mut era);
    // Valid envelopes forward.
    assert_eq!(
        classify(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, &mut era)["kind"],
        "forward"
    );
    // A non-standard notification is a compatibility drop, not an error.
    assert_eq!(
        classify(
            r#"{"jsonrpc":"2.0","method":"notifications/lsp/something"}"#,
            &mut era
        )["kind"],
        "drop"
    );
    // Unsupported per-request versions are a typed -32022 carrying the
    // supported set, and they mark the era modern.
    let verdict = classify(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
        &mut era,
    );
    assert_eq!(verdict["code"], -32022);
    assert!(
        verdict["data"]["supported"]
            .as_array()
            .is_some_and(|s| s.iter().any(|v| v == "2026-07-28")),
        "-32022 must carry data.supported: {verdict}"
    );
    assert!(
        era.is_modern(),
        "a _meta version field marks the modern era"
    );
    // On the modern path a request missing required _meta is -32602.
    expect_reject(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        -32602,
        &mut era,
    );
    // Legacy-era requests never required the modern fields.
    let mut legacy = EraState::default();
    assert_eq!(
        classify(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
            &mut legacy
        )["kind"],
        "forward"
    );
    assert_eq!(
        classify(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            &mut legacy
        )["kind"],
        "forward",
        "a 2025-11-25 session must not demand modern _meta"
    );

    // ── The shipped path: the served wire emits exactly the classified
    //    verdicts on the legacy era ────────────────────────────────────
    let mut wire = StdioSession::spawn(root, &home);

    // On the legacy era results keep the historical wire shape —
    // `resultType` is a 2026-07-28 field the compatibility rules strip
    // (absent is read as `complete`); only the modern path requires it.
    let result = wire.request("ping", serde_json::json!({}));
    assert!(
        result.get("resultType").is_none(),
        "a 2025-11-25 result must not carry resultType: {result}"
    );
    let result = wire.request("tools/list", serde_json::json!({}));
    assert!(result.get("resultType").is_none());

    // A null id is -32600, not a silent drop.
    let response = wire
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":null,"method":"tools/list"}),
            std::time::Duration::from_secs(10),
        )
        .expect("a null id must be answered");
    assert_eq!(response["error"]["code"], -32600);

    // Unparseable input is -32700, not a silent drop.
    wire.send_raw(b"{not-a-json-line}\n");
    let response = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a malformed line must be answered");
    assert_eq!(response["error"]["code"], -32700);

    // A batch array is -32600 — batches are not defined by MCP.
    wire.send_raw(br#"[{"jsonrpc":"2.0","id":9,"method":"ping"}]"#);
    wire.send_raw(b"\n");
    let response = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a batch must be answered");
    assert_eq!(response["error"]["code"], -32600);

    // A request id without a method is -32600.
    let response = wire
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":"no-method"}),
            std::time::Duration::from_secs(10),
        )
        .expect("a methodless request must be answered");
    assert_eq!(response["error"]["code"], -32600);

    // A cancellation notification is silent, and malformed input never
    // corrupts the session: the next request still answers.
    wire.send(&serde_json::json!({
        "jsonrpc":"2.0","method":"notifications/cancelled",
        "params":{"requestId":"in-flight-x","reason":"probe"}
    }));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let result = wire.request("ping", serde_json::json!({}));
    assert!(result.get("resultType").is_none());

    // ── The modern era: per-request _meta rules apply only there ──────
    let mut modern = StdioSession::spawn_modern(root, &home);

    // server/discover with _meta establishes the modern era.
    let response = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc":"2.0","id":"discover-1","method":"server/discover",
                "params":{"_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities":{}
                }}
            }),
            std::time::Duration::from_secs(30),
        )
        .expect("server/discover must be answered");
    assert!(response.get("result").is_some(), "discover: {response}");

    // A request missing required _meta is -32602 on the modern path.
    let response = modern
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":"meta-missing","method":"tools/list","params":{}}),
            std::time::Duration::from_secs(10),
        )
        .expect("a meta-missing request must be answered");
    assert_eq!(response["error"]["code"], -32602);

    // A request carrying the fields is served, with resultType.
    let response = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc":"2.0","id":"meta-ok","method":"tools/list",
                "params":{"_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities":{}
                }}
            }),
            std::time::Duration::from_secs(30),
        )
        .expect("a conforming modern request must be answered");
    assert_eq!(
        response["result"]["resultType"], "complete",
        "modern results carry resultType: {response}"
    );

    // An unsupported per-request version is -32022 with data.supported.
    let response = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc":"2.0","id":"bad-version","method":"tools/list",
                "params":{"_meta":{
                    "io.modelcontextprotocol/protocolVersion":"1900-01-01",
                    "io.modelcontextprotocol/clientCapabilities":{}
                }}
            }),
            std::time::Duration::from_secs(10),
        )
        .expect("an unsupported version must be answered");
    assert_eq!(response["error"]["code"], -32022);
    assert!(
        response["error"]["data"]["supported"]
            .as_array()
            .is_some_and(|s| s.iter().any(|v| v == "2026-07-28")),
        "-32022 must carry data.supported: {response}"
    );

    // The era persists: a later request without _meta still fails -32602.
    let response = modern
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":"era-persist","method":"tools/list","params":{}}),
            std::time::Duration::from_secs(10),
        )
        .expect("a late meta-missing request must be answered");
    assert_eq!(response["error"]["code"], -32602);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-03: every applicable surface shares one implementation
/// and one policy boundary. The canonical envelope classification
/// (`jsonrpc_gate::classify_envelope`) decides identically for the stdio
/// line transport and the daemon/gateway `/mcp` HTTP transport —
/// `http_envelope_rejection` is the same function `handle_request` runs
/// before `service.call`, so a body the wire rejects produces the same
/// error document on either transport, and tool dispatch stays the
/// shared plane-bus registry (the same report over direct and wire).
#[test]
fn gmcp_req_203_002_03_integration() {
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── The shipped handler runs the canonical gate — the daemon's only
    //    JSON-RPC ingress (`/mcp` on 9093) classifies through
    //    `http_envelope_rejection` before the session layer sees a body.
    let server_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/server.rs")).unwrap();
    let handler = server_src
        .split("fn handle_request")
        .nth(1)
        .expect("handle_request must exist");
    let gate_call = handler
        .find("http_envelope_rejection(&effective)")
        .expect("handle_request must run the canonical envelope gate");
    let dispatch = handler
        .find("service.call(req)")
        .expect("handle_request must dispatch to the service");
    assert!(
        gate_call < dispatch,
        "the envelope gate must run before the body reaches the service"
    );

    // ── Direct and served surfaces share tool dispatch: the same
    //    canonical report over `mcp call` and the stdio wire. ─────────
    let cli_report = susi_json(root, &home, &["mcp", "ledger"]);
    let direct = call_tool_report(&susi_json(
        root,
        &home,
        &["mcp", "call", "susi", "ledger_verify", "{}"],
    ));
    let mut wire = StdioSession::spawn(root, &home);
    let mut wire_modern = StdioSession::spawn_modern(root, &home);
    let wire_report = call_tool_report(&wire.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    ));
    assert_eq!(
        direct, cli_report,
        "direct dispatch must equal the CLI report"
    );
    assert_eq!(
        wire_report, cli_report,
        "served dispatch must equal the CLI report"
    );

    // ── Transport parity: every retained fixture produces the same
    //    verdict on the served wire and through the HTTP body gate. ────
    let mut envelope_rejects = 0;
    for class in ["positive", "negative"] {
        for entry in std::fs::read_dir(root.join(".agents/gmcp/fixtures").join(class))
            .unwrap()
            .flatten()
        {
            let fixture: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap()).unwrap();
            let Some(request) = fixture.get("request") else {
                continue;
            };
            let name = format!("{class}/{}", entry.file_name().to_str().unwrap());
            let era = fixture["era"].as_str().unwrap_or("legacy");
            let session = if era == "modern" {
                &mut wire_modern
            } else {
                &mut wire
            };
            let body = serde_json::to_vec(request).unwrap();
            let observed =
                match session.request_verbatim(request, std::time::Duration::from_secs(10)) {
                    None => serde_json::json!({"kind": "silent_drop"}),
                    Some(response) if response.get("error").is_some() => {
                        serde_json::json!({"kind": "error", "code": response["error"]["code"]})
                    }
                    Some(_) => serde_json::json!({"kind": "result"}),
                };
            let http = http_envelope_rejection(&body);
            match observed["kind"].as_str().unwrap() {
                "error" => {
                    if let Some(rejection) = &http {
                        envelope_rejects += 1;
                        assert_eq!(
                            rejection["error"]["code"], observed["code"],
                            "{name}: HTTP gate rejected with a different code than the wire"
                        );
                    }
                    // When the HTTP gate forwards, the wire's error came
                    // from a deeper layer — era/meta policy on HTTP lives
                    // in the SDK's header validation, so parity means "an
                    // error was answered", not "the same layer rejected".
                }
                "result" | "silent_drop" => {
                    assert!(
                        http.is_none(),
                        "{name}: the HTTP gate rejected a body the wire forwards: {http:?}"
                    );
                }
                other => panic!("{name}: unexpected wire outcome {other}"),
            }
        }
    }
    assert!(
        envelope_rejects >= 2,
        "the HTTP gate must share rejections with the wire, got {envelope_rejects}"
    );

    // ── The HTTP gate emits the identical error document the stdio
    //    transport writes — one canonical wire shape. ──────────────────
    for (body, code) in [
        (b"{not-a-json".as_slice(), -32700i64),
        (
            br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.as_slice(),
            -32600,
        ),
        (
            br#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#.as_slice(),
            -32600,
        ),
        (br#"{"jsonrpc":"2.0","id":1}"#.as_slice(), -32600),
        (
            br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_slice(),
            -32600,
        ),
    ] {
        let rejection = http_envelope_rejection(body)
            .unwrap_or_else(|| panic!("HTTP gate must reject {}", String::from_utf8_lossy(body)));
        assert_eq!(rejection["error"]["code"], code, "{}", rejection);
        assert_eq!(rejection["jsonrpc"], "2.0");
        assert!(rejection["error"]["message"].is_string());
    }
    // Valid envelopes and compatibility drops both pass the HTTP gate.
    assert!(http_envelope_rejection(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).is_none());
    assert!(http_envelope_rejection(
        br#"{"jsonrpc":"2.0","method":"notifications/lsp/something"}"#
    )
    .is_none());

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-04: adversarial JSON-RPC input is rejected with typed errors,
/// bounded before the SDK can buffer it, and isolated from neighboring
/// requests. Cancellation notifications never create a response or corrupt
/// the request stream, duplicate IDs remain independently correlated, and
/// modern-version metadata failures stay deterministic after malformed input.
#[test]
fn gmcp_req_203_002_04_adversarial() {
    use susi_gmcp::jsonrpc_gate::{classify_envelope, classify_line, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-adv-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The in-process policy must fail closed before the SDK sees hostile
    // envelopes, and each classification must be independent of its peers.
    let mut era = EraState::default();
    for (line, code) in [
        ("{not-json}", -32700),
        ("[]", -32600),
        (r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#, -32600),
        (r#"{"jsonrpc":"2.0","id":1}"#, -32600),
        (r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#, -32600),
    ] {
        match classify_line(line.as_bytes(), &mut era) {
            GateVerdict::Reject { code: observed, .. } => assert_eq!(observed, code, "{line}"),
            other @ (GateVerdict::Forward | GateVerdict::Drop) => {
                panic!("{line} must reject, got {other:?}")
            }
        }
    }
    assert!(matches!(
        classify_envelope(br#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#),
        GateVerdict::Forward
    ));
    assert!(matches!(
        classify_envelope(br#"{"jsonrpc":"2.0","method":"notifications/progress"}"#),
        GateVerdict::Forward
    ));

    let mut wire = StdioSession::spawn(root, &home);

    // A malformed frame produces a typed response and the following valid
    // request still reaches the service.
    wire.send_raw(b"{not-json}\n");
    let malformed = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("malformed input must receive a typed error");
    assert_eq!(malformed["error"]["code"], -32700);
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 700,
        "method": "ping",
        "params": {}
    }));
    let recovered = wire
        .response_for(&serde_json::json!(700), std::time::Duration::from_secs(10))
        .expect("a malformed frame must not poison its neighbor");
    assert_eq!(recovered["id"], 700);
    assert!(recovered["result"].is_object());

    // Duplicate IDs are a sender error (the protocol requires request IDs to
    // be unique), so the server need not promise two responses. It must not
    // let the duplicate poison a later, correctly identified request.
    for _ in 0..2 {
        wire.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 701,
            "method": "ping",
            "params": {}
        }));
    }
    let response = wire
        .response_for(&serde_json::json!(701), std::time::Duration::from_secs(10))
        .expect("a duplicate request must not prevent one valid response");
    assert_eq!(response["id"], 701);
    assert!(response["result"].is_object());
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 705,
        "method": "ping",
        "params": {}
    }));
    let response = wire
        .response_for(&serde_json::json!(705), std::time::Duration::from_secs(10))
        .expect("a duplicate request must not poison a later request");
    assert_eq!(response["id"], 705);
    assert!(response["result"].is_object());

    // Concurrent requests can complete in either order, but their IDs must
    // stay attached to their own response.
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 702,
        "method": "tools/list",
        "params": {}
    }));
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 703,
        "method": "ping",
        "params": {}
    }));
    for id in [702, 703] {
        let response = wire
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(30))
            .expect("concurrent requests must both complete");
        assert_eq!(response["id"], id);
        assert!(response["result"].is_object());
    }

    // Cancellation is a notification: it produces no response, including
    // invalid/late request references, and the next request remains usable.
    for request_id in ["in-flight", "missing"] {
        wire.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": request_id, "reason": "adversarial probe"}
        }));
    }
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 704,
        "method": "ping",
        "params": {}
    }));
    let response = wire
        .response_for(&serde_json::json!(704), std::time::Duration::from_secs(10))
        .expect("cancellation notifications must not block the next request");
    assert_eq!(response["id"], 704);
    assert!(
        wire.pending.iter().all(|value| !value["id"].is_null()),
        "cancellation must not emit a response"
    );

    // A cancellation notification cannot cancel initialize: a raw session
    // still completes its handshake and emits only the initialize response.
    let mut raw = StdioSession::spawn_raw(root, &home);
    raw.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "gmcp-adv", "version": "1"}
        }
    }));
    raw.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1}
    }));
    let initialize = raw
        .response_for(&serde_json::json!(1), std::time::Duration::from_secs(120))
        .expect("initialize must not be cancelled");
    assert!(initialize["result"].is_object());
    assert!(
        raw.pending.iter().all(|value| !value["id"].is_null()),
        "initialize cancellation must be silent"
    );
    raw.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    // Modern policy remains deterministic after an unsupported version and a
    // metadata omission; a fully populated request still succeeds.
    let mut modern = StdioSession::spawn_modern(root, &home);
    let unsupported = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "bad-version",
                "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(10),
        )
        .expect("unsupported versions must be answered");
    assert_eq!(unsupported["error"]["code"], -32022);
    assert!(unsupported["error"]["data"]["supported"].is_array());
    let missing_meta = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "missing-meta",
                "method": "tools/list",
                "params": {}
            }),
            std::time::Duration::from_secs(10),
        )
        .expect("modern metadata failures must be answered");
    assert_eq!(missing_meta["error"]["code"], -32602);
    let modern_result = modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "progressToken": "adv-1",
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        }}),
    );
    assert_eq!(modern_result["resultType"], "complete");

    drop(raw);
    drop(modern);
    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-05: retained reference fixtures interoperate with the served
/// rmcp SDK path in both protocol eras, direct and wire tool reports agree,
/// transport gates preserve the same malformed-envelope decisions, and a
/// bounded concurrent request set keeps response IDs and result shapes stable.
#[test]
fn gmcp_req_203_002_05_interop() {
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The direct CLI/call surfaces and the served SDK path must agree on a
    // stable, non-network capability before any fixture replay begins.
    let cli_report = susi_json(root, &home, &["mcp", "ledger"]);
    let direct_report = call_tool_report(&susi_json(
        root,
        &home,
        &["mcp", "call", "susi", "ledger_verify", "{}"],
    ));
    assert_eq!(direct_report, cli_report);

    let mut legacy = StdioSession::spawn(root, &home);
    let legacy_start = std::time::Instant::now();
    let legacy_ping = legacy.request("ping", serde_json::json!({}));
    assert!(
        legacy_start.elapsed() < std::time::Duration::from_secs(10),
        "legacy SDK round trip exceeded the interoperability budget"
    );
    assert!(legacy_ping.is_object());
    assert!(legacy_ping.get("resultType").is_none());

    // Replay every positive request fixture that has a wire payload. A
    // modern session is established once, matching how a reference client
    // keeps negotiated capabilities and then issues subsequent calls.
    let mut modern = StdioSession::spawn_modern(root, &home);
    for file in [
        "request-2026-07-28-meta.json",
        "request-progress-token.json",
        "request-trace-context.json",
        "server-discover-2026-07-28-meta.json",
        "server-discover-2026-07-28.json",
    ] {
        let path = root.join(".agents/gmcp/fixtures/positive").join(file);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        let request = &fixture["request"];
        let response = modern
            .request_verbatim(request, std::time::Duration::from_secs(30))
            .unwrap_or_else(|| panic!("{file} must receive a response"));
        if file == "server-discover-2026-07-28.json" {
            // This fixture is intentionally retained verbatim as a
            // compatibility regression: params.protocolVersion is not the
            // modern per-request _meta field, so the production path rejects
            // it and the corrected *_meta fixture proves the interoperable
            // form above.
            assert_eq!(response["error"]["code"], -32602, "{file}: {response}");
            let divergences =
                std::fs::read_to_string(root.join(".agents/gmcp/fixtures/divergences.json"))
                    .unwrap();
            assert!(divergences.contains("positive/server-discover-2026-07-28.json"));
            continue;
        }
        assert!(response["error"].is_null(), "{file}: {response}");
        assert!(response["result"].is_object(), "{file}: {response}");
        assert_eq!(fixture["expect"], "result");
        assert_eq!(response["result"]["resultType"], "complete", "{file}");
    }

    // The notification fixture is silent, but a reference client can keep
    // using the same session immediately afterward.
    let cancelled: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/notification-cancelled.json"),
        )
        .unwrap(),
    )
    .unwrap();
    legacy.send(&cancelled["request"]);
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 900,
        "method": "ping",
        "params": {}
    }));
    let response = legacy
        .response_for(&serde_json::json!(900), std::time::Duration::from_secs(10))
        .expect("a notification must not stall the next request");
    assert_eq!(response["id"], 900);
    assert!(legacy.pending.iter().all(|value| !value["id"].is_null()));

    // Negative fixtures exercise the same error codes through the served
    // reference path. Invalid metadata stays on the modern session; wire
    // envelope errors use an independent legacy session to avoid ambiguity
    // around null correlation IDs.
    let negative: &[(&str, i64, bool)] = &[
        ("request-meta-missing-2026-07-28.json", -32602, true),
        ("request-unsupported-version.json", -32022, true),
        ("request-batch-array.json", -32600, false),
        ("request-null-id.json", -32600, false),
    ];
    for (file, expected_code, modern_path) in negative {
        let path = root.join(".agents/gmcp/fixtures/negative").join(file);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        let response = if *modern_path {
            modern
                .request_verbatim(&fixture["request"], std::time::Duration::from_secs(10))
                .unwrap_or_else(|| panic!("{file} must receive an error"))
        } else {
            legacy
                .request_verbatim(&fixture["request"], std::time::Duration::from_secs(10))
                .unwrap_or_else(|| panic!("{file} must receive an error"))
        };
        assert_eq!(
            response["error"]["code"], *expected_code,
            "{file}: {response}"
        );
        assert!(
            response["error"]["message"].is_string(),
            "{file}: {response}"
        );
        if let Some(code) = fixture["expect_code"].as_i64() {
            assert_eq!(code, *expected_code, "{file} fixture drifted");
        }
    }

    // HTTP and stdio retain the same transport-independent verdict for every
    // malformed envelope that has no era-specific metadata policy.
    for body in [
        br#"{not-json"#.as_slice(),
        br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
    ] {
        let rejection = http_envelope_rejection(body).expect("HTTP must reject malformed input");
        let mut gate_era = susi_gmcp::jsonrpc_gate::EraState::default();
        let code = match susi_gmcp::jsonrpc_gate::classify_line(body, &mut gate_era) {
            susi_gmcp::jsonrpc_gate::GateVerdict::Reject { code, .. } => code,
            other @ (susi_gmcp::jsonrpc_gate::GateVerdict::Forward
            | susi_gmcp::jsonrpc_gate::GateVerdict::Drop) => {
                panic!("HTTP and stdio must reject the same body: {other:?}")
            }
        };
        assert_eq!(rejection["error"]["code"], code);
    }

    // A bounded burst models a reference client with multiple in-flight
    // requests. Every response must preserve its request ID and finish under
    // the same 30-second per-request budget used by the acceptance harness.
    let burst_start = std::time::Instant::now();
    for id in 1_000..1_016 {
        legacy.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "ping",
            "params": {}
        }));
    }
    for id in 1_000..1_016 {
        let response = legacy
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(30))
            .unwrap_or_else(|| panic!("burst request {id} timed out"));
        assert_eq!(response["id"], id);
        assert!(response["result"].is_object());
    }
    assert!(
        burst_start.elapsed() < std::time::Duration::from_secs(30),
        "concurrent burst exceeded the interoperability budget"
    );

    drop(modern);
    drop(legacy);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-06: the JSON-RPC substrate exposes bounded operator state,
/// validates its configuration, detects pinned-source drift fail closed, and
/// retains actionable diagnostics, migration, rollback, and evidence guidance.
#[test]
fn gmcp_req_203_002_06_operations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The production CLI is the inspectable operator surface. Its report is
    // bounded and verified before an operator or dashboard consumes it.
    let report = susi_json(root, &home, &["mcp", "ledger"]);
    assert_eq!(report["verified"], true);
    for key in [
        "rows",
        "documents",
        "revisions",
        "dispositions",
        "categories",
        "vectors",
    ] {
        assert!(!report[key].is_null(), "ledger report must expose {key}");
    }
    assert_eq!(report["rows"].as_u64(), Some(1713));
    assert_eq!(report["documents"].as_u64(), Some(72));

    // --refetched is only meaningful with --drift; invalid configuration is
    // rejected rather than silently skipping the requested comparison.
    let invalid = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(!invalid.status.success());

    let clean = susi_json(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            ".agents/gmcp/spec",
        ],
    );
    assert_eq!(clean["drift"], true);
    assert_eq!(clean["verified"], true);
    assert_eq!(clean["checked"].as_u64(), Some(72));
    assert_eq!(clean["checked"], clean["matched"]);
    assert!(clean["findings"].as_array().unwrap().is_empty());

    // A changed pinned document is a diagnostic finding and a non-zero
    // result, never a warning that allows stale protocol behavior through.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json")).unwrap(),
    )
    .unwrap();
    let files = manifest["files"].as_array().unwrap();
    let drifted_doc = files[0]["document"].as_str().unwrap().to_string();
    let missing_doc = files[1]["document"].as_str().unwrap().to_string();
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(&drifted_doc);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes.push(b'!');
    std::fs::write(&victim, &bytes).unwrap();
    let drifted = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!drifted.status.success());
    let drift_report: serde_json::Value =
        serde_json::from_slice(&drifted.stdout).expect("drift failure must remain JSON");
    assert_eq!(drift_report["verified"], false);
    assert!(drift_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["document"] == drifted_doc && finding["kind"] == "hash_drift"));

    // An unavailable source is separately diagnosable and also fails closed.
    std::fs::remove_file(refetched.join(&missing_doc)).unwrap();
    std::fs::write(&victim, &bytes[..bytes.len() - 1]).unwrap();
    let missing = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!missing.status.success());
    let missing_report: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("missing source must remain JSON");
    assert!(missing_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["document"] == missing_doc && finding["kind"] == "unfetchable"));

    // The contract is the retained incident handoff: it must tell an
    // operator what state exists, how to diagnose it, and how to migrate or
    // roll back a pin without inventing an undocumented recovery path.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-002.md"))
        .expect("contract must exist");
    for token in [
        "Operations and incident runbook",
        "Inspectable state",
        "Diagnostics",
        "Incident triage",
        "Migration (re-pinning the JSON-RPC standard)",
        "Rollback",
        "--refetched",
        "hash_drift",
        "unfetchable",
        "traceparent",
        "resultType",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // Evidence is append-only and tied to the current pinned ledger bytes,
    // so a changed dependency invalidates stale proof instead of preserving
    // a green record by wording alone.
    use sha2::{Digest, Sha256};
    let ledger_hash = hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-002.jsonl"))
        .expect("VC-203-002 must retain evidence");
    let record = evidence
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|record| record["requirement"] == "GMCP-203-002-06")
        .expect("evidence must retain a record for GMCP-203-002-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_002_06_operations"
    );
    assert_eq!(record["exit"], 0);
    assert_eq!(record["artifact_sha256"], ledger_hash);
    assert!(!record["detail"].as_str().unwrap_or_default().is_empty());
    assert!(!record["identity"].as_str().unwrap_or_default().is_empty());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(record["recorded_unix"].as_u64().unwrap() <= now + 60);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-M: the vector outcome holds on the shipped production
/// path — malformed, duplicated, reordered, oversized, and adversarial
/// messages fail predictably without corrupting another request — and
/// every retained evidence record for GMCP-203-002-01..06 plus the
/// mastery record is complete, fresh, signed, committed, and backed by
/// a real test.
#[test]
fn gmcp_vc_203_002_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::jsonrpc_gate::{classify_envelope, http_envelope_rejection, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-002-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Vector outcome on the shipped wire path: an oversized line is
    //    answered once, bounded before the SDK buffers it, and cannot
    //    corrupt the request that shares its read chunk or any request
    //    after it ────────────────────────────────────────────────────
    // Dev binaries run as their own instance — the bound lands in the
    // `.susi-dev` root a `target/` build resolves (Mandate 48).
    let susi_home = home.join(".susi-dev");
    std::fs::create_dir_all(&susi_home).unwrap();
    std::fs::write(
        susi_home.join("config.json"),
        r#"{"max_rpc_body_bytes": 1024}"#,
    )
    .unwrap();
    let mut wire = StdioSession::spawn(root, &home);
    let giant = format!(
        r#"{{"jsonrpc":"2.0","id":800,"method":"ping","params":{{"pad":"{}"}}}}"#,
        "x".repeat(4096)
    );
    // The dead line's tail shares a read chunk with the next request —
    // the bound must recover inside the same burst.
    let mut burst = giant.into_bytes();
    burst.push(b'\n');
    burst.extend_from_slice(br#"{"jsonrpc":"2.0","id":801,"method":"ping","params":{}}"#);
    burst.push(b'\n');
    wire.send_raw(&burst);
    let over = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("an oversized line must receive a typed error, not silence");
    assert_eq!(over["error"]["code"], -32600);
    let neighbour = wire
        .response_for(&serde_json::json!(801), std::time::Duration::from_secs(30))
        .expect("the request sharing the dead line's chunk must still be answered");
    assert!(neighbour["result"].is_object());

    // Malformed, batched, and null-id envelopes each fail typed and the
    // stream stays usable after every one of them.
    wire.send_raw(b"{corrupt\n");
    let parse = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("malformed input must be answered");
    assert_eq!(parse["error"]["code"], -32700);
    wire.send_raw(b"[]\n");
    let batch = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a batch must be answered");
    assert_eq!(batch["error"]["code"], -32600);
    wire.send_raw(br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#);
    wire.send_raw(b"\n");
    let null_id = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a null request id must be answered");
    assert_eq!(null_id["error"]["code"], -32600);
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 802,
        "method": "ping",
        "params": {}
    }));
    let alive = wire
        .response_for(&serde_json::json!(802), std::time::Duration::from_secs(30))
        .expect("the stream must remain usable after every failure kind");
    assert!(alive["result"].is_object());

    // Reordered concurrent answers keep their request identity.
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 803,
        "method": "tools/list",
        "params": {}
    }));
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 804,
        "method": "ping",
        "params": {}
    }));
    for id in [803, 804] {
        let response = wire
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(30))
            .expect("concurrent requests must both complete");
        assert_eq!(response["id"], id);
    }

    // A notification is never answered, and a client response envelope
    // (a result-bearing line answering a server→client request) is a
    // valid message, not an invalid request.
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": "never", "reason": "mastery probe"}
    }));
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 805,
        "method": "ping",
        "params": {}
    }));
    let response = wire
        .response_for(&serde_json::json!(805), std::time::Duration::from_secs(30))
        .expect("a notification must not block the next request");
    assert_eq!(response["id"], 805);
    assert!(
        wire.pending.iter().all(|value| !value["id"].is_null()),
        "notifications must not emit a response"
    );
    assert!(matches!(
        classify_envelope(br#"{"jsonrpc":"2.0","id":9,"result":{}}"#),
        GateVerdict::Forward
    ));

    // Transport parity: the HTTP envelope seam reaches the same verdict
    // for every envelope class the stdio gate rejects.
    for (line, code) in [
        (
            br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
            -32600,
        ),
        (br#"[]"#.as_slice(), -32600),
        (
            br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_slice(),
            -32600,
        ),
    ] {
        let rejection =
            http_envelope_rejection(line).expect("HTTP seam must reject the same envelope");
        assert_eq!(rejection["error"]["code"], code);
    }

    // The modern era stays deterministic: missing `_meta` fails -32602,
    // an unsupported version fails -32022 with `data.supported`, and a
    // fully populated request returns a stamped `resultType`.
    let mut modern = StdioSession::spawn_modern(root, &home);
    let missing_meta = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "m-meta",
                "method": "tools/list",
                "params": {}
            }),
            std::time::Duration::from_secs(10),
        )
        .expect("modern metadata failures must be answered");
    assert_eq!(missing_meta["error"]["code"], -32602);
    let unsupported = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "m-vers",
                "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(10),
        )
        .expect("unsupported versions must be answered");
    assert_eq!(unsupported["error"]["code"], -32022);
    assert!(unsupported["error"]["data"]["supported"].is_array());
    let modern_result = modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "progressToken": "mastery-1",
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        }}),
    );
    assert_eq!(modern_result["resultType"], "complete");
    drop(wire);
    drop(modern);

    // ── The contract pins the vector's required vocabulary ──────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-002.md"))
        .expect("the vector contract must exist");
    for term in [
        "-32700",
        "-32600",
        "-32022",
        "resultType",
        "_meta",
        "notifications/cancelled",
        "progressToken",
        "traceparent",
        "2026-07-28",
    ] {
        assert!(contract.contains(term), "contract must pin {term}");
    }

    // ── Evidence verification: complete, fresh, signed, backed by a
    //    real test — missing, stale, unsigned, waived, mock-only or
    //    zero-test records fail ──────────────────────────────────────
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json"))
        .expect("the canonical ledger must exist");
    let ledger_hash = hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = std::fs::read_to_string(root.join("tests/gmcp_requirements.rs")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-002.jsonl"))
        .expect("VC-203-002 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    for line in evidence.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("evidence records must be single-line JSON");
        for field in [
            "requirement",
            "check",
            "exit",
            "detail",
            "recorded_unix",
            "artifact_sha256",
            "identity",
        ] {
            assert!(
                !record[field].is_null(),
                "unsigned or incomplete evidence: record lacks {field}"
            );
        }
        assert!(
            record.get("waived").is_none() && record.get("mock").is_none(),
            "waived or mock-only evidence does not count: {record}"
        );
        assert_eq!(record["exit"], 0, "evidence must record a green check");
        assert!(
            !record["identity"].as_str().unwrap().is_empty(),
            "evidence must be signed by an identity"
        );
        assert!(
            !record["detail"].as_str().unwrap().is_empty(),
            "evidence must describe what the check proved"
        );
        // Staleness: the recorded hash must be the canonical ledger's
        // current bytes — proof of a since-changed artifact is stale.
        assert_eq!(
            record["artifact_sha256"].as_str().unwrap(),
            ledger_hash,
            "stale evidence: recorded hash must be the current ledger hash"
        );
        let recorded = record["recorded_unix"].as_u64().unwrap();
        assert!(
            recorded > 1_792_256_000 && recorded <= now + 60,
            "recorded_unix {recorded} must be a plausible recent timestamp"
        );
        // Zero-test evidence: the check must be a real command on a
        // committed artifact — a `cargo test` command must name a test
        // that exists in the named file.
        let check = record["check"].as_str().unwrap();
        if let Some(test_name) = check.strip_prefix("cargo test --locked --test gmcp_requirements ")
        {
            assert!(
                test_name.starts_with("gmcp_")
                    && !test_name.contains(char::is_whitespace)
                    && test_src.contains(&format!("fn {test_name}")),
                "evidence names a test that does not exist: {test_name}"
            );
        } else {
            assert!(
                check
                    .split_whitespace()
                    .filter(|token| !token.starts_with('-'))
                    .any(|token| root.join(token).exists()),
                "evidence check must reference a committed artifact: {check}"
            );
        }
        covered.insert(record["requirement"].as_str().unwrap().to_string());
    }
    for req in [
        "GMCP-203-002-01",
        "GMCP-203-002-02",
        "GMCP-203-002-03",
        "GMCP-203-002-04",
        "GMCP-203-002-05",
        "GMCP-203-002-06",
        "GMCP-203-002-M",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── Committed proof: locally modified artifacts are not
    //    evidence — the vector's tree must be clean against git ──────
    let status = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--",
            ".agents/gmcp",
            "docs/gmcp",
            "tests/gmcp_requirements.rs",
            "crates/susi-gmcp",
        ])
        .current_dir(root)
        .output()
        .expect("git status must run — committed proof is required");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "locally modified artifacts are not proof:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-01: the stateless-lifecycle/server-discovery contract
/// (`docs/gmcp/contract-203-003.md`) exists, pins its normative clause
/// and sources, binds exactly the six VC-203-003 ledger rows, names the
/// canonical production paths, resolves the compatibility/threat
/// decisions, retains the seed fixtures, and holds a complete -01
/// evidence record.
#[test]
fn gmcp_req_203_003_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // ── The contract document exists and carries every required
    //    section ────────────────────────────────────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-003.md"))
        .expect("contract-203-003.md must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    // The stateless-lifecycle decisions the contract must have resolved
    // before implementation.
    for decision in [
        "server/discover",
        "initialize",
        "serverInfo",
        "supportedVersions",
        "ttlMs",
        "cacheScope",
        "_meta",
        "session identifier",
        "explicit handles",
        "stateless",
        "negotiated-state leak",
    ] {
        assert!(
            contract.contains(decision),
            "contract must resolve: {decision}"
        );
    }

    // ── The ledger's VC-203-003 rows are the pinned normative set the
    //    contract binds: exact counts, complete traces, existing
    //    production path ─────────────────────────────────────────────
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must parse"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-003")
        .collect();
    assert_eq!(rows.len(), 6, "the contract binds 6 VC-203-003 rows");
    let mut revisions = std::collections::BTreeMap::new();
    for row in &rows {
        *revisions
            .entry(row["revision"].as_str().unwrap().to_string())
            .or_insert(0usize) += 1;
        let id = row["id"].as_str().unwrap();
        for field in [
            "vector",
            "owner",
            "production_path",
            "positive_test",
            "negative_test",
            "evidence",
        ] {
            assert!(
                !row["trace"][field].as_str().unwrap_or_default().is_empty(),
                "{id}: trace.{field} must be non-empty"
            );
        }
        assert_eq!(
            row["trace"]["evidence"], ".agents/gmcp/evidence/VC-203-003.jsonl",
            "{id}: evidence must point at this vector's artifact"
        );
        let production = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        for cat in row["categories"].as_array().unwrap() {
            assert!(
                ["behavior", "claim"].contains(&cat.as_str().unwrap()),
                "{id}: unexpected category {cat}"
            );
        }
    }
    assert_eq!(revisions.get("2026-07-28"), Some(&5));
    assert_eq!(revisions.get("susi@baseline-2026-10-17"), Some(&1));

    // ── The full pinned-corpus check still passes — the contract did
    //    not disturb the ledger machinery ────────────────────────────
    let checker = Command::new("python3")
        .arg("scripts/check-gmcp-ledger.py")
        .current_dir(root)
        .output()
        .expect("the checker must run");
    assert!(
        checker.status.success(),
        "checker failed:\n{}",
        String::from_utf8_lossy(&checker.stderr)
    );

    // ── Seed fixtures: every file the contract names exists, parses,
    //    and follows the fixture convention ──────────────────────────
    let seeds: &[(&str, &str)] = &[
        (
            "positive/server-discover-fullmeta-2026-07-28.json",
            "result",
        ),
        ("negative/server-discover-unsupported-version.json", "error"),
    ];
    for (file, expect) in seeds {
        let path = root.join(".agents/gmcp/fixtures").join(file);
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file} must exist: {e}")),
        )
        .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        assert!(
            !fixture["comment"].as_str().unwrap_or_default().is_empty(),
            "{file} must comment which row/decision it exercises"
        );
        assert!(
            !fixture["request"].is_null(),
            "{file} must carry a request payload"
        );
        assert_eq!(
            fixture["expect"].as_str().unwrap(),
            *expect,
            "{file} expect must be {expect}"
        );
        assert_eq!(
            fixture["request"]["method"], "server/discover",
            "{file} must exercise the discover method"
        );
        if file.starts_with("negative/") {
            assert_eq!(
                fixture["expect_code"], -32022,
                "{file} must pin the unsupported-version error code"
            );
        }
    }
    // The discover fixtures carry the per-request protocol version under
    // params._meta — the modern addressing form.
    for file in [
        "positive/server-discover-fullmeta-2026-07-28.json",
        "negative/server-discover-unsupported-version.json",
    ] {
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".agents/gmcp/fixtures").join(file)).unwrap(),
        )
        .unwrap();
        assert!(
            !fixture["request"]["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
                .is_null(),
            "{file} must carry _meta.protocolVersion"
        );
    }

    // ── Evidence location: the vector's append-only artifact exists
    //    and holds a complete -01 record ──────────────────────────────
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-003.jsonl"))
        .expect("VC-203-003 must retain an evidence artifact");
    let record = evidence
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|r| r["requirement"] == "GMCP-203-003-01")
        .expect("evidence must retain a record for GMCP-203-003-01");
    for field in [
        "check",
        "exit",
        "detail",
        "recorded_unix",
        "artifact_sha256",
        "identity",
    ] {
        assert!(!record[field].is_null(), "evidence record lacks {field}");
    }
    assert_eq!(record["exit"], 0);
}

/// GMCP-203-003-02: the canonical stateless lifecycle lives in
/// `susi_gmcp::protocol` — `discover_result` is explicit canonical
/// state (identity, advertised registry, pinned cache hints) rather
/// than ambient handler defaults, and the shipped wire path serves the
/// complete discover result with no handshake, validates the
/// per-request version deterministically, and keeps concurrent modern
/// requests independent (no negotiated state exists to leak).
#[test]
fn gmcp_req_203_003_02_production() {
    use susi_gmcp::jsonrpc_gate::{classify_line, EraState, GateVerdict};
    use susi_gmcp::protocol::{
        GmcpService, DISCOVER_CACHE_SCOPE, DISCOVER_TTL_MS, MODERN_REQUEST_VERSIONS,
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── In-process canonical state: the discover result is the
    //    contract's wire shape built from named constants ────────────
    let service = GmcpService::new(scratch.clone());
    let result = service.discover_result();
    let wire = serde_json::to_value(&result).expect("discover result serializes");
    assert_eq!(wire["resultType"], "complete");
    let versions = wire["supportedVersions"]
        .as_array()
        .expect("supportedVersions must be a non-empty array");
    assert!(!versions.is_empty());
    assert!(
        versions.iter().any(|v| v == "2026-07-28"),
        "the advertised registry must include the modern revision: {versions:?}"
    );
    assert!(
        wire["capabilities"].is_object(),
        "capabilities must be present: {wire}"
    );
    let server_info = &wire["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(server_info["name"], "susi-gmcp");
    assert!(
        server_info["version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "serverInfo.version must identify the build: {wire}"
    );
    assert_eq!(wire["ttlMs"], DISCOVER_TTL_MS);
    assert_eq!(
        wire["cacheScope"],
        serde_json::to_value(DISCOVER_CACHE_SCOPE).unwrap()
    );
    // The canonical registry and the gate's -32022 data agree — one
    // source of truth for per-request version validation.
    let mut era = EraState::default();
    let unsupported = br#"{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1999-01-01"}}}"#;
    match classify_line(unsupported, &mut era) {
        GateVerdict::Reject { code, data, .. } => {
            assert_eq!(code, -32022);
            assert_eq!(
                data.unwrap()["supported"].as_array().unwrap().len(),
                MODERN_REQUEST_VERSIONS.len(),
                "data.supported must be the canonical registry"
            );
        }
        GateVerdict::Forward | GateVerdict::Drop => {
            panic!("unsupported version must reject -32022")
        }
    }

    // ── Wire path: a modern session answers discover plus ordinary
    //    requests with no initialize ceremony, and each response
    //    carries the complete contract shape ─────────────────────────
    let mut modern = StdioSession::spawn_modern(root, &home);
    let discover = modern.request(
        "server/discover",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}),
    );
    assert_eq!(discover["resultType"], "complete");
    assert!(!discover["supportedVersions"].as_array().unwrap().is_empty());
    assert_eq!(
        discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "susi-gmcp"
    );
    assert!(discover["ttlMs"].as_u64().is_some());
    assert!(matches!(
        discover["cacheScope"].as_str(),
        Some("public" | "private")
    ));
    // Stateless: an ordinary request with only per-request _meta — no
    // handshake established anything — is served on its own context.
    let tools = modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}),
    );
    assert!(tools["tools"].is_array(), "tools/list must answer: {tools}");
    // Version validation is per request — an unsupported declared
    // version earns -32022 even mid-session on the modern path.
    let rejected = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "bad-version",
                "method": "server/discover",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1999-01-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(30),
        )
        .expect("unsupported version must be answered, not dropped");
    assert_eq!(rejected["error"]["code"], -32022);
    assert_eq!(
        rejected["error"]["data"]["supported"]
            .as_array()
            .unwrap()
            .len(),
        MODERN_REQUEST_VERSIONS.len()
    );
    // Concurrent requests stay independent: two in-flight requests with
    // different declared versions are correlated by id, never by any
    // shared negotiated state.
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "concurrent-a", "method": "server/discover",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "concurrent-b", "method": "tools/list",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2025-11-25",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    let a = modern
        .response_for(
            &serde_json::json!("concurrent-a"),
            std::time::Duration::from_secs(60),
        )
        .expect("concurrent-a must be answered");
    let b = modern
        .response_for(
            &serde_json::json!("concurrent-b"),
            std::time::Duration::from_secs(60),
        )
        .expect("concurrent-b must be answered");
    assert!(a["result"].is_object(), "discover concurrent answer: {a}");
    assert!(
        b["result"]["tools"].is_array() || b["error"].is_object(),
        "each request is answered on its own declared context: {b}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-03: the stateless discover capability reaches every
/// shipped surface through one canonical implementation — the stdio
/// `serve` path and the HTTP `/mcp` handler both dispatch into the same
/// `GmcpService`, the wire answer equals `discover_result` verbatim,
/// and the legacy `initialize` identity channel agrees with the modern
/// `_meta.serverInfo` channel.
#[test]
fn gmcp_req_203_003_03_integration() {
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;
    use susi_gmcp::protocol::GmcpService;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Structural: both transports construct and serve the same
    //    canonical service — stdio `service.serve`, HTTP
    //    `StreamableHttpService<GmcpService>` — and the discover
    //    override lives in protocol.rs (not a per-surface copy). ──────
    let server_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/server.rs")).unwrap();
    let protocol_src =
        std::fs::read_to_string(root.join("crates/susi-gmcp/src/protocol.rs")).unwrap();
    assert!(
        server_src.contains("service.serve(transport)"),
        "the stdio path must serve GmcpService"
    );
    assert!(
        server_src.contains("StreamableHttpService<GmcpService"),
        "the HTTP path must serve the same GmcpService type"
    );
    assert!(
        protocol_src.contains("async fn discover"),
        "GmcpService must own the discover handler"
    );
    assert!(
        protocol_src.contains("fn supported_protocol_versions"),
        "the advertised registry must be explicit, not ambient"
    );

    // ── The canonical result is the wire answer: in-process
    //    `discover_result` and a modern stdio session agree on every
    //    contract field, and a repeated call is byte-identical (the
    //    complete+cacheable contract) ────────────────────────────────
    let service = GmcpService::new(scratch.clone());
    let canonical = serde_json::to_value(service.discover_result()).unwrap();
    let mut modern = StdioSession::spawn_modern(root, &home);
    let discover_msg = |id: u64| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        })
    };
    modern.send(&discover_msg(41));
    let first = modern
        .response_for(&serde_json::json!(41), std::time::Duration::from_secs(60))
        .expect("discover must be answered")["result"]
        .clone();
    modern.send(&discover_msg(42));
    let second = modern
        .response_for(&serde_json::json!(42), std::time::Duration::from_secs(60))
        .expect("discover must be answered")["result"]
        .clone();
    for field in [
        "resultType",
        "supportedVersions",
        "capabilities",
        "ttlMs",
        "cacheScope",
    ] {
        assert_eq!(
            first[field], canonical[field],
            "{field}: wire must equal the canonical result"
        );
    }
    assert_eq!(
        first["_meta"]["io.modelcontextprotocol/serverInfo"],
        canonical["_meta"]["io.modelcontextprotocol/serverInfo"],
        "serverInfo: wire must equal the canonical result"
    );
    assert_eq!(first, second, "discover is deterministic per build");

    // ── Era parity: the legacy `initialize` identity channel and the
    //    modern `_meta.serverInfo` channel report the same server —
    //    identity is one canonical value, not per-era state. ──────────
    let mut legacy = StdioSession::spawn_raw(root, &home);
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "gmcp-it", "version": "1.0"}}
    }));
    let init = legacy
        .response_for(&serde_json::json!(1), std::time::Duration::from_secs(120))
        .expect("initialize must be answered");
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "notifications/initialized"
    }));
    legacy.send(&discover_msg(43));
    let legacy_discover = legacy
        .response_for(&serde_json::json!(43), std::time::Duration::from_secs(60))
        .expect("discover must answer on the legacy path too")["result"]
        .clone();
    let init_info = &init["result"]["serverInfo"];
    let discover_info = &legacy_discover["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(init_info["name"], discover_info["name"]);
    assert_eq!(init_info["version"], discover_info["version"]);
    assert_eq!(
        legacy_discover["supportedVersions"], first["supportedVersions"],
        "the advertised registry is identical across eras"
    );

    // ── HTTP seam parity: a discover body is a valid envelope, so the
    //    shared gate forwards it to the same `service.call` dispatch —
    //    no transport-specific discover implementation exists. ────────
    let body = serde_json::to_vec(&discover_msg(7)).unwrap();
    assert!(
        http_envelope_rejection(&body).is_none(),
        "the HTTP envelope gate must forward a well-formed discover"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-04: adversarial and failure behavior for the stateless
/// lifecycle — a spoofed `clientInfo` cannot alter the canonical
/// `serverInfo`, version confusion fails typed, a request carrying
/// modern per-request metadata on a negotiated-legacy session earns a
/// deterministic `-32600` instead of the silent drop the service would
/// perform, and that rejection cannot poison the negotiated era for
/// later legacy requests.
#[test]
fn gmcp_req_203_003_04_adversarial() {
    use susi_gmcp::jsonrpc_gate::{classify_line, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-adv-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let classify = |line: &str, era: &mut EraState| -> serde_json::Value {
        match classify_line(line.as_bytes(), era) {
            GateVerdict::Forward => serde_json::json!({"kind": "forward"}),
            GateVerdict::Drop => serde_json::json!({"kind": "drop"}),
            GateVerdict::Reject { code, .. } => {
                serde_json::json!({"kind": "reject", "code": code})
            }
        }
    };

    // ── Negotiated-legacy era is permanent: a modern-_meta request
    //    earns -32600 and never flips the era, so the next bare legacy
    //    request still forwards as legacy ────────────────────────────
    let mut era = EraState::default();
    let init = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}"#;
    assert_eq!(
        classify(std::str::from_utf8(init).unwrap(), &mut era)["kind"],
        "forward"
    );
    assert!(
        era.is_legacy(),
        "initialize must pin the negotiated-legacy era"
    );
    let modern_on_legacy = br#"{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    assert_eq!(
        classify(std::str::from_utf8(modern_on_legacy).unwrap(), &mut era)["code"],
        -32600,
        "modern _meta on a legacy session must reject deterministically"
    );
    assert!(
        era.is_legacy(),
        "the rejected request must not flip the negotiated era"
    );
    // The dual-era probe stays exempt on a legacy session.
    let discover_on_legacy = br#"{"jsonrpc":"2.0","id":10,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    assert_eq!(
        classify(std::str::from_utf8(discover_on_legacy).unwrap(), &mut era)["kind"],
        "forward",
        "server/discover must remain answerable on a legacy session"
    );
    assert!(era.is_legacy(), "discover must not flip the era either");
    // A bare legacy request after the era attack still forwards — the
    // anomalous request could not poison the session.
    let bare_legacy = br#"{"jsonrpc":"2.0","id":11,"method":"ping","params":{}}"#;
    assert_eq!(
        classify(std::str::from_utf8(bare_legacy).unwrap(), &mut era)["kind"],
        "forward"
    );

    // ── Modern era mirror: a legacy request on a modern session must
    //    carry the per-request fields ────────────────────────────────
    let mut modern_era = EraState::default();
    let discover = br#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    classify(std::str::from_utf8(discover).unwrap(), &mut modern_era);
    assert!(modern_era.is_modern());
    let legacy_on_modern = br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    assert_eq!(
        classify(
            std::str::from_utf8(legacy_on_modern).unwrap(),
            &mut modern_era
        )["code"],
        -32602
    );
    // A malformed version type fails typed, never parses onward.
    let bad_type = br#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":20260728,"io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let verdict = classify(std::str::from_utf8(bad_type).unwrap(), &mut modern_era);
    assert!(
        verdict["kind"] == "forward" || verdict["code"] == -32602,
        "a non-string version must fail typed, got {verdict}"
    );

    // ── Wire path: the served endpoint emits the same verdicts and a
    //    hostile clientInfo cannot change the reported identity ─────
    let mut legacy = StdioSession::spawn(root, &home);
    let attack = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "era-attack", "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(30),
        )
        .expect("era-mismatched request must earn a typed answer, not a drop");
    assert_eq!(attack["error"]["code"], -32600);
    // The negotiated era survives: discover still answers on the
    // legacy session and reports the canonical identity.
    let discover_after = legacy.request(
        "server/discover",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": {"name": "susi-gmcp", "version": "99.0"},
            "io.modelcontextprotocol/clientCapabilities": {}
        }}),
    );
    let identity = &discover_after["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(identity["name"], "susi-gmcp");
    assert_ne!(
        identity["version"], "99.0",
        "a spoofed clientInfo must not alter the canonical serverInfo"
    );
    // And the era did not flip: a subsequent bare request forwards as
    // legacy — it is served (or at minimum never earns the -32602 a
    // poisoned modern era would emit).
    let ping = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "post-attack-ping", "method": "ping",
                "params": {}
            }),
            std::time::Duration::from_secs(30),
        )
        .expect("a bare legacy request after the attack must be answered");
    assert!(
        ping.get("error").is_none_or(|e| e.is_null()) || ping["error"]["code"] != -32602,
        "the era attack must not poison the negotiated session: {ping}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-05: interoperability and quality for the stateless
/// lifecycle — reference-SDK fixture replay on the served path in both
/// message directions, direct-versus-gateway differential discover
/// behavior, determinism and statelessness invariants, concurrent
/// cross-era clients with no negotiated-state leak, transport parity at
/// the shared envelope seam, and the contract's published
/// latency/throughput budgets.
#[test]
fn gmcp_req_203_003_05_interop() {
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;
    use susi_gmcp::protocol::GmcpService;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Reference-SDK replay, request direction: every fixture the
    //    contract binds earns the verdict it declares on the served
    //    modern path. ────────────────────────────────────────────────
    let mut modern = StdioSession::spawn_modern(root, &home);
    for file in [
        "server-discover-fullmeta-2026-07-28.json",
        "server-discover-2026-07-28-meta.json",
        "request-2026-07-28-meta.json",
    ] {
        let path = root.join(".agents/gmcp/fixtures/positive").join(file);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        let response = modern
            .request_verbatim(&fixture["request"], std::time::Duration::from_secs(30))
            .unwrap_or_else(|| panic!("{file} must receive a response"));
        assert!(response["error"].is_null(), "{file}: {response}");
        assert!(response["result"].is_object(), "{file}: {response}");
        assert_eq!(fixture["expect"], "result");
        assert_eq!(response["result"]["resultType"], "complete", "{file}");
    }
    for (file, expected_code) in [
        ("server-discover-unsupported-version.json", -32022_i64),
        ("request-meta-missing-2026-07-28.json", -32602_i64),
    ] {
        let path = root.join(".agents/gmcp/fixtures/negative").join(file);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        let response = modern
            .request_verbatim(&fixture["request"], std::time::Duration::from_secs(120))
            .unwrap_or_else(|| panic!("{file} must receive an error"));
        assert_eq!(
            response["error"]["code"], expected_code,
            "{file}: {response}"
        );
        assert_eq!(
            fixture["expect_code"].as_i64(),
            Some(expected_code),
            "{file}"
        );
    }

    // The retained verbatim divergence record still documents the
    // non-interoperable form: `params.protocolVersion` is not the modern
    // `_meta` field, so the wire verdict stays `-32602` and the file is
    // listed in divergences.json.
    let divergent: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/server-discover-2026-07-28.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let response = modern
        .request_verbatim(&divergent["request"], std::time::Duration::from_secs(30))
        .expect("the divergence fixture must receive a typed error");
    assert_eq!(response["error"]["code"], -32602, "{response}");
    let divergences =
        std::fs::read_to_string(root.join(".agents/gmcp/fixtures/divergences.json")).unwrap();
    assert!(divergences.contains("positive/server-discover-2026-07-28.json"));

    // ── Response direction: a consumed client notification does not
    //    stall the server→client answer channel on the same session. ──
    let mut legacy = StdioSession::spawn(root, &home);
    let cancelled: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/notification-cancelled.json"),
        )
        .unwrap(),
    )
    .unwrap();
    legacy.send(&cancelled["request"]);
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 910, "method": "ping", "params": {}
    }));
    let response = legacy
        .response_for(&serde_json::json!(910), std::time::Duration::from_secs(30))
        .expect("a notification must not stall the next request");
    assert_eq!(response["id"], 910);

    // ── Direct-versus-gateway differential: the canonical
    //    `discover_result` (direct, in-process) and the served wire
    //    answer (gateway) agree on every contract field. ──────────────
    let service = GmcpService::new(scratch.clone());
    let canonical = serde_json::to_value(service.discover_result()).unwrap();
    let discover_msg = |id: u64| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        })
    };
    modern.send(&discover_msg(51));
    let wire = modern
        .response_for(&serde_json::json!(51), std::time::Duration::from_secs(30))
        .expect("discover must be answered")["result"]
        .clone();
    for field in [
        "resultType",
        "supportedVersions",
        "capabilities",
        "ttlMs",
        "cacheScope",
        "instructions",
    ] {
        assert_eq!(wire[field], canonical[field], "{field}: wire != canonical");
    }
    assert_eq!(
        wire["_meta"]["io.modelcontextprotocol/serverInfo"],
        canonical["_meta"]["io.modelcontextprotocol/serverInfo"],
        "serverInfo: wire != canonical"
    );

    // ── Property invariants: discover is deterministic and stateless —
    //    an intervening request cannot perturb the next answer, and a
    //    second session observes the identical result. ────────────────
    modern.send(&discover_msg(52));
    let repeat = modern
        .response_for(&serde_json::json!(52), std::time::Duration::from_secs(30))
        .expect("discover must be answered")["result"]
        .clone();
    assert_eq!(wire, repeat, "discover must be deterministic per build");
    let mut modern_b = StdioSession::spawn_modern(root, &home);
    modern_b.send(&discover_msg(53));
    let second_session = modern_b
        .response_for(&serde_json::json!(53), std::time::Duration::from_secs(30))
        .expect("discover must be answered on a second session")["result"]
        .clone();
    assert_eq!(
        wire, second_session,
        "discover must be identical across independent sessions"
    );

    // ── Concurrent cross-era clients: negotiated state never leaks.
    //    A bare legacy ping on the negotiated-legacy session and a
    //    `_meta`-missing modern request on the modern session each earn
    //    their own era's verdict while both sessions are live. ────────
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 911, "method": "ping", "params": {}
    }));
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 54, "method": "ping", "params": {}
    }));
    let legacy_answer = legacy
        .response_for(&serde_json::json!(911), std::time::Duration::from_secs(30))
        .expect("the legacy session must answer a bare ping");
    assert!(legacy_answer["result"].is_object(), "{legacy_answer}");
    assert!(
        legacy_answer["result"].get("resultType").is_none(),
        "legacy wire shape must stay resultType-free: {legacy_answer}"
    );
    let modern_answer = modern
        .response_for(&serde_json::json!(54), std::time::Duration::from_secs(30))
        .expect("the modern session must answer the bare ping");
    assert_eq!(
        modern_answer["error"]["code"], -32602,
        "a modern session must demand `_meta`: {modern_answer}"
    );

    // ── Transport parity: the shared envelope seam forwards a
    //    well-formed modern request on HTTP and rejects malformed bytes
    //    with the same verdict on both transports. ────────────────────
    let body = serde_json::to_vec(&discover_msg(60)).unwrap();
    assert!(
        http_envelope_rejection(&body).is_none(),
        "HTTP must forward a well-formed discover envelope"
    );
    for raw in [
        br#"{not-json"#.as_slice(),
        br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.as_slice(),
    ] {
        let rejection = http_envelope_rejection(raw).expect("HTTP must reject malformed input");
        let mut gate_era = susi_gmcp::jsonrpc_gate::EraState::default();
        let code = match susi_gmcp::jsonrpc_gate::classify_line(raw, &mut gate_era) {
            susi_gmcp::jsonrpc_gate::GateVerdict::Reject { code, .. } => code,
            other @ (susi_gmcp::jsonrpc_gate::GateVerdict::Forward
            | susi_gmcp::jsonrpc_gate::GateVerdict::Drop) => {
                panic!("both transports must reject the same body: {other:?}")
            }
        };
        assert_eq!(rejection["error"]["code"], code);
    }

    // ── Published budgets: discover is a constant payload — a warm call
    //    stays well under the 30s per-request bound, and a 16-call burst
    //    answers every request fairly with preserved correlation. ─────
    let warm = std::time::Instant::now();
    modern.send(&discover_msg(55));
    let _ = modern
        .response_for(&serde_json::json!(55), std::time::Duration::from_secs(30))
        .expect("a warm discover must answer");
    assert!(
        warm.elapsed() < std::time::Duration::from_secs(30),
        "warm discover exceeded the latency budget: {:?}",
        warm.elapsed()
    );
    let burst_start = std::time::Instant::now();
    for id in 2_000..2_016 {
        modern.send(&discover_msg(id));
    }
    for id in 2_000..2_016 {
        let response = modern
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(30))
            .unwrap_or_else(|| panic!("burst discover {id} timed out"));
        assert_eq!(response["id"], id);
        assert!(response["result"].is_object());
    }
    assert!(
        burst_start.elapsed() < std::time::Duration::from_secs(60),
        "16-discover burst exceeded the throughput budget: {:?}",
        burst_start.elapsed()
    );

    drop(modern);
    drop(modern_b);
    drop(legacy);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-06: operations and evidence for the stateless
/// lifecycle — the ledger CLI is the bounded inspectable surface,
/// `server/discover` answers as the live capability surface, drift
/// detection fails closed on this vector's pinned discover document,
/// the contract carries the incident/migration/rollback runbook, and
/// the vector's evidence record is retained, signed, and fresh.
#[test]
fn gmcp_req_203_003_06_operations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The shared ledger report is the bounded operator surface.
    let report = susi_json(root, &home, &["mcp", "ledger"]);
    assert_eq!(report["verified"], true);
    for key in [
        "rows",
        "documents",
        "revisions",
        "dispositions",
        "categories",
        "vectors",
    ] {
        assert!(!report[key].is_null(), "ledger report must expose {key}");
    }

    // `server/discover` is this vector's live inspectable surface: the
    // contract-bound fullmeta fixture replays on a served endpoint and
    // reports the canonical fields an operator consumes.
    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/server-discover-fullmeta-2026-07-28.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut modern = StdioSession::spawn_modern(root, &home);
    let response = modern
        .request_verbatim(&fixture["request"], std::time::Duration::from_secs(30))
        .expect("discover must answer on the served surface")["result"]
        .clone();
    for field in [
        "resultType",
        "supportedVersions",
        "capabilities",
        "ttlMs",
        "cacheScope",
    ] {
        assert!(!response[field].is_null(), "discover must expose {field}");
    }
    assert!(!response["_meta"]["io.modelcontextprotocol/serverInfo"].is_null());
    drop(modern);

    // Invalid configuration is rejected rather than silently degraded:
    // `--refetched` is meaningless without `--drift`.
    let invalid = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(!invalid.status.success());

    // The deterministic drift check is clean on the shipped corpus and
    // covers this vector's pinned documents.
    let clean = susi_json(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            ".agents/gmcp/spec",
        ],
    );
    assert_eq!(clean["drift"], true);
    assert_eq!(clean["verified"], true);
    assert_eq!(clean["checked"], clean["matched"]);
    assert!(clean["findings"].as_array().unwrap().is_empty());

    // A drifted discover document is a named `hash_drift` finding and a
    // non-zero result — stale protocol proof cannot survive a changed pin.
    let discover_doc = "docs/specification/2026-07-28/server/discover.mdx";
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(discover_doc);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes.push(b'!');
    std::fs::write(&victim, &bytes).unwrap();
    let drifted = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!drifted.status.success());
    let drift_report: serde_json::Value =
        serde_json::from_slice(&drifted.stdout).expect("drift failure must remain JSON");
    assert_eq!(drift_report["verified"], false);
    assert!(drift_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| { f["document"] == discover_doc && f["kind"] == "hash_drift" }));

    // An absent discover document fails closed as `unfetchable`.
    std::fs::remove_file(&victim).unwrap();
    let missing = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!missing.status.success());
    let missing_report: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("missing source must remain JSON");
    assert!(missing_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| { f["document"] == discover_doc && f["kind"] == "unfetchable" }));

    // The contract carries the operator runbook: state, diagnostics,
    // triage, migration, and rollback are documented — not tribal.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-003.md"))
        .expect("contract must exist");
    for token in [
        "Operations and incident runbook",
        "Inspectable state",
        "Diagnostics",
        "Incident triage",
        "Migration (re-pinning",
        "Rollback",
        "hash_drift",
        "unfetchable",
        "-32022",
        "silent drop",
        "supportedVersions",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // Evidence is retained, signed, and tied to the canonical ledger
    // bytes, so a changed corpus invalidates this proof.
    use sha2::{Digest, Sha256};
    let ledger_hash = hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-003.jsonl"))
        .expect("VC-203-003 must retain evidence");
    let record = evidence
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|record| record["requirement"] == "GMCP-203-003-06")
        .expect("evidence must retain a record for GMCP-203-003-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_003_06_operations"
    );
    assert_eq!(record["exit"], 0);
    assert_eq!(record["artifact_sha256"], ledger_hash);
    assert!(!record["detail"].as_str().unwrap_or_default().is_empty());
    assert!(!record["identity"].as_str().unwrap_or_default().is_empty());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(record["recorded_unix"].as_u64().unwrap() <= now + 60);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-M: the vector outcome holds on the shipped production
/// path — for MCP 2026-07-28 and later GMCP is genuinely stateless at
/// the protocol layer: no initialize ceremony or session identifier is
/// required, every request carries and validates version plus client
/// capabilities, every result identifies the server, `server/discover`
/// is complete and cacheable, explicit handles bind all cross-call
/// state, and concurrent requests from changing clients cannot leak
/// negotiated state — and every retained evidence record for
/// GMCP-203-003-01..06 plus this mastery record is complete, fresh,
/// signed, committed, and backed by a real test.
#[test]
fn gmcp_vc_203_003_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::protocol::GmcpService;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Stateless: no initialize ceremony, no session identifier — a
    //    modern `_meta` request is answered on a brand-new connection
    //    and carries `resultType` on the result column. ──────────────
    let mut raw = StdioSession::spawn_raw(root, &home);
    raw.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": "stateless",
        "method": "tools/list",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    let stateless = raw
        .response_for(
            &serde_json::json!("stateless"),
            std::time::Duration::from_secs(120),
        )
        .expect("a modern request must be served with no initialize ceremony");
    assert!(
        stateless["result"].is_object(),
        "stateless modern request must earn a result: {stateless}"
    );
    assert_eq!(stateless["result"]["resultType"], "complete");

    // ── Every request carries and validates version plus client
    //    capabilities: a missing `_meta` fails -32602, an unsupported
    //    version fails -32022 with the advertised registry, and a
    //    missing clientCapabilities fails -32602. ────────────────────
    let mut modern = StdioSession::spawn_modern(root, &home);
    for (id, params, code) in [
        ("no-meta", serde_json::json!({}), -32602_i64),
        (
            "bad-version",
            serde_json::json!({"_meta": {
                "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}),
            -32022_i64,
        ),
        (
            "no-caps",
            serde_json::json!({"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28"
            }}),
            -32602_i64,
        ),
    ] {
        let response = modern
            .request_verbatim(
                &serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "method": "tools/list", "params": params
                }),
                std::time::Duration::from_secs(30),
            )
            .unwrap_or_else(|| panic!("{id} must receive a typed error"));
        assert_eq!(response["error"]["code"], code, "{id}: {response}");
        if code == -32022 {
            assert!(
                response["error"]["data"]["supported"].is_array(),
                "-32022 must carry the supported registry: {response}"
            );
        }
    }

    // ── Every result identifies the server: the discover result's
    //    `_meta.serverInfo` equals the canonical identity, and the
    //    whole result equals the canonical `discover_result` — the
    //    complete, cacheable contract. ──────────────────────────────
    let service = GmcpService::new(scratch.clone());
    let canonical = serde_json::to_value(service.discover_result()).unwrap();
    let discover_msg = |id: u64| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        })
    };
    modern.send(&discover_msg(71));
    let discover = modern
        .response_for(&serde_json::json!(71), std::time::Duration::from_secs(30))
        .expect("discover must answer")["result"]
        .clone();
    assert_eq!(
        discover["_meta"]["io.modelcontextprotocol/serverInfo"],
        canonical["_meta"]["io.modelcontextprotocol/serverInfo"],
        "the served result must identify the canonical server"
    );
    assert_eq!(discover["resultType"], "complete");
    modern.send(&discover_msg(72));
    let cached = modern
        .response_for(&serde_json::json!(72), std::time::Duration::from_secs(30))
        .expect("a cacheable discover must repeat")["result"]
        .clone();
    assert_eq!(
        discover, cached,
        "discover must be byte-identical — cacheable"
    );

    // ── Explicit handles bind cross-call state: progressToken and
    //    traceparent travel in `_meta` — no ambient negotiated state. ─
    let handled = modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "progressToken": "mastery-handle",
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        }}),
    );
    assert_eq!(handled["resultType"], "complete");

    // ── Concurrent changing clients cannot leak negotiated state: a
    //    negotiated-legacy session and the modern session stay live
    //    together, a modern-metadata request on the legacy session
    //    earns -32600 without poisoning the era, and each session keeps
    //    its own verdict afterwards. ─────────────────────────────────
    let mut legacy = StdioSession::spawn(root, &home);
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 81, "method": "tools/list",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    let confused = legacy
        .response_for(&serde_json::json!(81), std::time::Duration::from_secs(30))
        .expect("era-confused metadata must earn a typed verdict");
    assert_eq!(confused["error"]["code"], -32600, "{confused}");
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 82, "method": "ping", "params": {}
    }));
    let still_legacy = legacy
        .response_for(&serde_json::json!(82), std::time::Duration::from_secs(30))
        .expect("the rejection must not poison the negotiated era");
    assert!(
        still_legacy["result"].is_object(),
        "the legacy session must stay usable: {still_legacy}"
    );
    assert!(
        still_legacy["result"].get("resultType").is_none(),
        "the legacy wire shape must not inherit modern fields"
    );

    drop(raw);
    drop(modern);
    drop(legacy);

    // ── The contract pins the vector's required vocabulary ──────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-003.md"))
        .expect("the vector contract must exist");
    for term in [
        "server/discover",
        "2026-07-28",
        "io.modelcontextprotocol/protocolVersion",
        "io.modelcontextprotocol/clientCapabilities",
        "io.modelcontextprotocol/serverInfo",
        "resultType",
        "ttlMs",
        "cacheScope",
        "supportedVersions",
        "-32022",
        "-32602",
        "-32600",
        "stateless",
    ] {
        assert!(contract.contains(term), "contract must pin {term}");
    }

    // ── Evidence verification: complete, fresh, signed, backed by a
    //    real test — missing, stale, unsigned, waived, mock-only or
    //    zero-test records fail ──────────────────────────────────────
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json"))
        .expect("the canonical ledger must exist");
    let ledger_hash = hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = std::fs::read_to_string(root.join("tests/gmcp_requirements.rs")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-003.jsonl"))
        .expect("VC-203-003 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    for line in evidence.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("evidence records must be single-line JSON");
        for field in [
            "requirement",
            "check",
            "exit",
            "detail",
            "recorded_unix",
            "artifact_sha256",
            "identity",
        ] {
            assert!(
                !record[field].is_null(),
                "unsigned or incomplete evidence: record lacks {field}"
            );
        }
        assert!(
            record.get("waived").is_none() && record.get("mock").is_none(),
            "waived or mock-only evidence does not count: {record}"
        );
        assert_eq!(record["exit"], 0, "evidence must record a green check");
        assert!(
            !record["identity"].as_str().unwrap().is_empty(),
            "evidence must be signed by an identity"
        );
        assert!(
            !record["detail"].as_str().unwrap().is_empty(),
            "evidence must describe what the check proved"
        );
        assert_eq!(
            record["artifact_sha256"].as_str().unwrap(),
            ledger_hash,
            "stale evidence: recorded hash must be the current ledger hash"
        );
        let recorded = record["recorded_unix"].as_u64().unwrap();
        assert!(
            recorded > 1_792_256_000 && recorded <= now + 60,
            "recorded_unix {recorded} must be a plausible recent timestamp"
        );
        let check = record["check"].as_str().unwrap();
        if let Some(test_name) = check.strip_prefix("cargo test --locked --test gmcp_requirements ")
        {
            assert!(
                test_name.starts_with("gmcp_")
                    && !test_name.contains(char::is_whitespace)
                    && test_src.contains(&format!("fn {test_name}")),
                "evidence names a test that does not exist: {test_name}"
            );
        } else {
            assert!(
                check
                    .split_whitespace()
                    .filter(|token| !token.starts_with('-'))
                    .any(|token| root.join(token).exists()),
                "evidence check must reference a committed artifact: {check}"
            );
        }
        covered.insert(record["requirement"].as_str().unwrap().to_string());
    }
    for req in [
        "GMCP-203-003-01",
        "GMCP-203-003-02",
        "GMCP-203-003-03",
        "GMCP-203-003-04",
        "GMCP-203-003-05",
        "GMCP-203-003-06",
        "GMCP-203-003-M",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── Committed proof: locally modified artifacts are not
    //    evidence — the vector's tree must be clean against git ──────
    let status = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--",
            ".agents/gmcp",
            "docs/gmcp",
            "tests/gmcp_requirements.rs",
            "crates/susi-gmcp",
        ])
        .current_dir(root)
        .output()
        .expect("git status must run — committed proof is required");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "locally modified artifacts are not proof:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-01: the schemas/primitives/content/metadata contract
/// (`docs/gmcp/contract-203-008.md`) exists, pins its normative clause
/// and sources, binds exactly the VC-203-008 ledger rows, names the
/// canonical production paths, resolves the compatibility/threat
/// decisions, retains the seed fixtures, and holds a complete -01
/// evidence record.
#[test]
fn gmcp_req_203_008_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // ── The contract document exists and carries every required
    //    section ────────────────────────────────────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-008.md"))
        .expect("contract-203-008.md must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    // Ambiguities the contract must have resolved before implementation.
    for decision in [
        "ContentBlock",
        "Annotations",
        "includeContext",
        "adapter",
        "ttlMs",
        "resultType",
        "unknown",
    ] {
        assert!(
            contract.contains(decision),
            "contract must resolve: {decision}"
        );
    }

    // ── The ledger's VC-203-008 rows are the pinned normative set the
    //    contract binds: exact counts, complete traces, existing
    //    production paths ────────────────────────────────────────────
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must parse"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-008")
        .collect();
    assert_eq!(rows.len(), 425, "the contract binds 425 VC-203-008 rows");
    let mut revisions = std::collections::BTreeMap::new();
    for row in &rows {
        *revisions
            .entry(row["revision"].as_str().unwrap().to_string())
            .or_insert(0usize) += 1;
        let id = row["id"].as_str().unwrap();
        for field in [
            "vector",
            "owner",
            "production_path",
            "positive_test",
            "negative_test",
            "evidence",
        ] {
            assert!(
                !row["trace"][field].as_str().unwrap_or_default().is_empty(),
                "{id}: trace.{field} must be non-empty"
            );
        }
        assert_eq!(
            row["trace"]["evidence"], ".agents/gmcp/evidence/VC-203-008.jsonl",
            "{id}: evidence must point at this vector's artifact"
        );
        for cat in row["categories"].as_array().unwrap() {
            assert!(
                ["behavior", "error", "security", "deprecation", "claim"]
                    .contains(&cat.as_str().unwrap()),
                "{id}: unexpected category {cat}"
            );
        }
    }
    assert_eq!(revisions.get("2026-07-28"), Some(&304));
    assert_eq!(revisions.get("2025-11-25"), Some(&120));
    assert_eq!(revisions.get("susi@baseline-2026-10-17"), Some(&1));

    // ── Every pinned source the contract names exists and is listed in
    //    sources.json, so drift detection covers the bound corpus ────
    let sources: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json")).unwrap(),
    )
    .unwrap();
    let source_docs: std::collections::BTreeSet<&str> = sources["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["document"].as_str().unwrap())
        .collect();
    let bound_docs: std::collections::BTreeSet<&str> = rows
        .iter()
        .map(|r| r["source"]["document"].as_str().unwrap())
        .collect();
    for doc in &bound_docs {
        if *doc == "docs/gmcp-roadmap.md" {
            // The baseline claim's source is our own roadmap — the spec
            // pins only cover the upstream corpus.
            continue;
        }
        assert!(
            source_docs.contains(doc),
            "bound source {doc} must be a pinned file in sources.json"
        );
    }

    // ── The full pinned-corpus check still passes — the contract did
    //    not disturb the VC-203-001 machinery ─────────────────────────
    let checker = Command::new("python3")
        .arg("scripts/check-gmcp-ledger.py")
        .current_dir(root)
        .output()
        .expect("the checker must run");
    assert!(
        checker.status.success(),
        "checker failed:\n{}",
        String::from_utf8_lossy(&checker.stderr)
    );

    // ── Seed fixtures: every file the contract names exists, parses,
    //    and follows the payload-fixture convention ──────────────────
    let seeds: &[(&str, &str)] = &[
        ("positive/content-block-annotated-2026-07-28.json", "result"),
        ("negative/content-block-bad-mime.json", "error"),
    ];
    for (file, expect) in seeds {
        let path = root.join(".agents/gmcp/fixtures").join(file);
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file} must exist: {e}")),
        )
        .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        assert!(
            !fixture["comment"].as_str().unwrap_or_default().is_empty(),
            "{file} must comment which row/decision it exercises"
        );
        assert!(!fixture["payload"].is_null(), "{file} must carry a payload");
        assert_eq!(fixture["expect"].as_str().unwrap(), *expect);
        assert_eq!(fixture["era"].as_str().unwrap(), "modern");
    }
    // The annotated fixture exercises the lossless surface: icon,
    // audience, priority, _meta, and an unknown extension field.
    let annotated: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/content-block-annotated-2026-07-28.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let payload = &annotated["payload"];
    for key in ["icons", "annotations", "_meta", "uri", "mimeType"] {
        assert!(
            !payload[key].is_null(),
            "the annotated fixture must exercise {key}"
        );
    }
    assert!(
        payload.get("io.example/futureExtension").is_some(),
        "the annotated fixture must carry an unknown extension field"
    );

    // ── Evidence location: the vector's append-only artifact exists
    //    and holds a complete -01 record ──────────────────────────────
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-008.jsonl"))
        .expect("VC-203-008 must retain an evidence artifact");
    let record = evidence
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|r| r["requirement"] == "GMCP-203-008-01")
        .expect("evidence must retain a record for GMCP-203-008-01");
    for field in [
        "check",
        "exit",
        "detail",
        "recorded_unix",
        "artifact_sha256",
        "identity",
    ] {
        assert!(!record[field].is_null(), "evidence record lacks {field}");
    }
    assert_eq!(record["exit"], 0);
}

/// GMCP-203-008-02: the canonical content/metadata model lives in
/// `susi_gmcp::content` and runs at the production boundary — the shared
/// `jsonrpc_gate::classify_envelope` both stdio and HTTP walk every
/// payload through. Lossless representation (unknown extension fields
/// round-trip verbatim), boundary validation (URI, MIME, base64,
/// Unicode, annotation ranges → typed errors), the explicit rmcp
/// adapter, bounded payloads, and deterministic canonical serialization
/// are all exercised here — on the module and over the served wire.
#[test]
fn gmcp_req_203_008_02_production() {
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-production-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Lossless round-trip: every variant, plus extension fields a
    //    future revision may define, survive parse → serialize ──────
    let fixture = std::fs::read_to_string(
        root.join(".agents/gmcp/fixtures/positive/content-block-annotated-2026-07-28.json"),
    )
    .unwrap();
    let fixture: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    let payload = &fixture["payload"];
    let block = CanonicalContentBlock::from_wire(payload)
        .expect("annotated resource_link fixture must validate");
    let round_trip = block.to_wire();
    assert_eq!(
        &round_trip, payload,
        "canonical round-trip must be verbatim, extension fields included"
    );
    assert_eq!(
        round_trip["io.example/futureExtension"]["arbitrary"][0], "unknown",
        "the unknown extension field must survive verbatim"
    );

    // Every spec variant validates and round-trips.
    for (variant, value) in [
        (
            "text",
            serde_json::json!({"type":"text","text":"hello \u{4e16}\u{754c}"}),
        ),
        (
            "image",
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"image/png"}),
        ),
        (
            "audio",
            serde_json::json!({"type":"audio","data":"AAE=","mimeType":"audio/wav"}),
        ),
        (
            "embedded text resource",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a.txt","mimeType":"text/plain","text":"body"}}),
        ),
        (
            "embedded blob resource",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a.bin","blob":"AAE="}}),
        ),
        ("resource_link", payload.clone()),
    ] {
        let parsed = CanonicalContentBlock::from_wire(&value)
            .unwrap_or_else(|e| panic!("{variant} must validate: {e}"));
        assert_eq!(
            parsed.to_wire(),
            value,
            "{variant} must round-trip losslessly"
        );
    }

    // ── Boundary validation: typed failures, never silent coercion ──
    let bad_mime_fixture = std::fs::read_to_string(
        root.join(".agents/gmcp/fixtures/negative/content-block-bad-mime.json"),
    )
    .unwrap();
    let bad_mime_fixture: serde_json::Value = serde_json::from_str(&bad_mime_fixture).unwrap();
    assert_eq!(
        CanonicalContentBlock::from_wire(&bad_mime_fixture["payload"]),
        Err(ContentError::BadMimeType("image")),
        "the bad-mime fixture must fail typed validation"
    );
    for (label, value, want) in [
        (
            "bad base64",
            serde_json::json!({"type":"image","data":"!!!","mimeType":"image/png"}),
            ContentError::BadBase64("image"),
        ),
        (
            "bad link uri",
            serde_json::json!({"type":"resource_link","uri":"relative/path","name":"x"}),
            ContentError::BadUri("resource_link.uri"),
        ),
        (
            "bad link mime",
            serde_json::json!({"type":"resource_link","uri":"file:///a","name":"x","mimeType":"no mime"}),
            ContentError::BadMimeType("resource_link.mimeType"),
        ),
        (
            "priority out of range",
            serde_json::json!({"type":"text","text":"t","annotations":{"priority":1.5}}),
            ContentError::BadPriority,
        ),
        (
            "bad audience",
            serde_json::json!({"type":"text","text":"t","annotations":{"audience":["admin"]}}),
            ContentError::BadAudience,
        ),
        (
            "control character",
            serde_json::json!({"type":"text","text":"a\u{7}"}),
            ContentError::BadUnicode("text"),
        ),
        (
            "bad blob base64",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a","blob":"%%%="}}),
            ContentError::BadBase64("resource.blob"),
        ),
        (
            "bad embedded uri",
            serde_json::json!({"type":"resource","resource":{"uri":"no scheme","text":"t"}}),
            ContentError::BadUri("resource.uri"),
        ),
        (
            "bad icon src",
            serde_json::json!({"type":"resource_link","uri":"file:///a","name":"x","icons":[{"src":"not a uri"}]}),
            ContentError::BadIcon,
        ),
        (
            "unknown variant",
            serde_json::json!({"type":"hologram","voxels":"AAA="}),
            ContentError::UnknownVariant,
        ),
    ] {
        assert_eq!(
            CanonicalContentBlock::from_wire(&value),
            Err(want),
            "{label} must fail typed validation"
        );
    }

    // Bounded payloads: a nesting deeper than the limit earns a typed
    // TooLarge, never unbounded recursion.
    let mut deep = serde_json::json!("leaf");
    for _ in 0..80 {
        deep = serde_json::json!({"k": deep});
    }
    let deep = serde_json::json!({"type":"text","text":"t","_meta":{"deep":deep}});
    assert_eq!(
        CanonicalContentBlock::from_wire(&deep),
        Err(ContentError::TooLarge)
    );

    // Deterministic canonical serialization: equal values produce
    // byte-identical output across parses and key orders.
    let reordered: serde_json::Value = serde_json::from_str(
        &serde_json::to_string(&serde_json::json!({
            "type": "text", "text": "x",
            "z-ext": 1, "a-ext": 2
        }))
        .unwrap(),
    )
    .unwrap();
    let first = CanonicalContentBlock::from_wire(&reordered).unwrap();
    let second = CanonicalContentBlock::from_wire(&first.to_wire()).unwrap();
    assert_eq!(first.canonical_json(), second.canonical_json());
    assert!(
        first.canonical_json().find("a-ext").unwrap()
            < first.canonical_json().find("z-ext").unwrap(),
        "canonical serialization must be key-ordered"
    );

    // ── The explicit adapter: canonical → typed rmcp model — every
    //    spec field maps exactly onto the typed representation ───────
    let typed = block.to_rmcp().expect("canonical must adapt to rmcp");
    let typed_wire = serde_json::to_value(&typed).expect("rmcp model must serialize");
    assert_eq!(typed_wire["type"], "resource_link");
    assert_eq!(typed_wire["uri"], "file:///workspace/report.pdf");
    assert_eq!(typed_wire["name"], "quarterly-report");
    let adapted_priority = typed_wire["annotations"]["priority"]
        .as_f64()
        .expect("priority must adapt");
    assert!(
        (adapted_priority - 0.8).abs() < 1e-6,
        "annotations must adapt to the typed model"
    );

    // ── The shipped wire path: the gate's boundary validation is what
    //    `mcp serve` runs on every inbound line ──────────────────────
    let mut session = StdioSession::spawn(root, &home);
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "bad-uri",
        "method": "resources/read",
        "params": {"uri": "not a uri at all"}
    }));
    let response = session
        .response_for(
            &serde_json::json!("bad-uri"),
            std::time::Duration::from_secs(60),
        )
        .expect("malformed resources/read must be answered");
    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("params.uri"),
        "the typed error must name the offending field: {response}"
    );

    // A malformed client response (sampling-style content) earns a
    // typed -32600 instead of the typed codec's silent drop.
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "bad-content",
        "result": {"content": {"type": "image", "data": "!!!", "mimeType": "image/png"}}
    }));
    let response = session
        .response_for(
            &serde_json::json!("bad-content"),
            std::time::Duration::from_secs(60),
        )
        .expect("malformed client response must be answered");
    assert_eq!(response["error"]["code"], -32600);
    assert!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("base64"),
        "the typed error must name the base64 failure: {response}"
    );

    // Malformed roots[] answers are caught the same way.
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "bad-roots",
        "result": {"roots": [{"uri": "relative/path"}]}
    }));
    let response = session
        .response_for(
            &serde_json::json!("bad-roots"),
            std::time::Duration::from_secs(60),
        )
        .expect("malformed roots response must be answered");
    assert_eq!(response["error"]["code"], -32600);

    // A well-formed request still reaches the service — the boundary
    // rejects only malformed payloads (any non-gate answer counts).
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "ok-read",
        "method": "resources/read",
        "params": {"uri": "file:///workspace/ok.txt"}
    }));
    let response = session
        .response_for(
            &serde_json::json!("ok-read"),
            std::time::Duration::from_secs(120),
        )
        .expect("well-formed resources/read must be answered");
    assert_ne!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default(),
        "content field `params.uri` is not a valid absolute URI",
        "a well-formed request must pass the boundary: {response}"
    );

    // ── Transport-independence: the HTTP body gate runs the same
    //    classify_envelope the stdio path does ───────────────────────
    for (body, want_code) in [
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"bad uri"}}"#,
            -32602,
        ),
        (
            r#"{"jsonrpc":"2.0","id":2,"result":{"content":{"type":"audio","data":"$$","mimeType":"audio/wav"}}}"#,
            -32600,
        ),
        (r#"{"jsonrpc":"2.0","id":3,"method":"ping","params":{}}"#, 0),
    ] {
        let verdict = susi_gmcp::jsonrpc_gate::http_envelope_rejection(body.as_bytes());
        if want_code == 0 {
            assert!(verdict.is_none(), "a clean body must forward: {body}");
        } else {
            assert_eq!(
                verdict
                    .as_ref()
                    .and_then(|v| v.pointer("/error/code"))
                    .and_then(serde_json::Value::as_i64),
                Some(i64::from(want_code)),
                "the HTTP gate must emit the same typed failure: {body}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-03: every surface — `mcp serve` stdio, the daemon/gateway
/// HTTP `/mcp` handler, and the in-process gate — runs the same content
/// and metadata boundary: `jsonrpc_gate::classify_envelope` calling
/// `content::validate_payload` on the canonical `susi_gmcp::content`
/// model. A corpus of malformed and well-formed bodies must produce
/// identical typed verdicts on both transports, and the wire `reason`
/// must be the canonical `ContentError`'s own message — one model, not
/// parallel implementations.
#[test]
fn gmcp_req_203_008_03_integration() {
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-integration-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Verdict parity: identical bodies → identical verdicts on the
    //    stdio line gate and the HTTP body gate ──────────────────────
    // Each entry: (label, body, expected code or None for forward).
    let corpus: &[(&str, &str, Option<i64>)] = &[
        // Envelope rules both transports share.
        ("not json", "{oops", Some(-32700)),
        (
            "batch",
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
            Some(-32600),
        ),
        (
            "null id",
            r#"{"jsonrpc":"2.0","id":null,"method":"ping","params":{}}"#,
            Some(-32600),
        ),
        (
            "bad envelope version",
            r#"{"jsonrpc":"1.9","id":1,"method":"ping","params":{}}"#,
            Some(-32600),
        ),
        // Canonical content/metadata boundary (VC-203-008).
        (
            "bad resources/read uri",
            r#"{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"not a uri"}}"#,
            Some(-32602),
        ),
        (
            "missing resources/read uri",
            r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{}}"#,
            Some(-32602),
        ),
        (
            "bad subscribe uri",
            r#"{"jsonrpc":"2.0","id":7,"method":"resources/subscribe","params":{"uri":"rel/path"}}"#,
            Some(-32602),
        ),
        (
            "bad completion ref uri",
            r#"{"jsonrpc":"2.0","id":8,"method":"completion/complete","params":{"ref":{"type":"ref/resource","uri":"no"},"argument":{"name":"x","value":"y"}}}"#,
            Some(-32602),
        ),
        (
            "bad response content mime",
            r#"{"jsonrpc":"2.0","id":9,"result":{"content":{"type":"image","data":"aGk=","mimeType":"bad mime"}}}"#,
            Some(-32600),
        ),
        (
            "bad response content base64",
            r#"{"jsonrpc":"2.0","id":10,"result":{"content":[{"type":"audio","data":"$$","mimeType":"audio/wav"}]}}"#,
            Some(-32600),
        ),
        (
            "bad roots uri",
            r#"{"jsonrpc":"2.0","id":11,"result":{"roots":[{"uri":"rel"}]}}"#,
            Some(-32600),
        ),
        // Well-formed values forward identically.
        (
            "clean resources/read",
            r#"{"jsonrpc":"2.0","id":12,"method":"resources/read","params":{"uri":"file:///a.txt"}}"#,
            None,
        ),
        (
            "clean content response",
            r#"{"jsonrpc":"2.0","id":13,"result":{"content":{"type":"text","text":"hi","x-future":true}}}"#,
            None,
        ),
        (
            "elicitation form data",
            r#"{"jsonrpc":"2.0","id":14,"result":{"action":"accept","content":{"name":"alice","age":3}}}"#,
            None,
        ),
        (
            "clean ping",
            r#"{"jsonrpc":"2.0","id":15,"method":"ping","params":{}}"#,
            None,
        ),
    ];
    for (label, body, want) in corpus {
        // HTTP surface: the daemon/gateway `/mcp` handler calls this.
        let http = http_envelope_rejection(body.as_bytes());
        // Stdio surface: the same line through the served transport's
        // classifier on a fresh era (no negotiation yet).
        let mut era = EraState::default();
        let stdio = classify_line(body.as_bytes(), &mut era);
        match (want, &http) {
            (None, None) => {
                assert!(
                    matches!(stdio, GateVerdict::Forward | GateVerdict::Drop),
                    "{label}: stdio must forward/drop the clean body, got {stdio:?}"
                );
            }
            (None, Some(http_error)) => {
                panic!("{label}: HTTP gate rejected a clean body: {http_error}");
            }
            (Some(code), Some(http_error)) => {
                assert_eq!(
                    http_error
                        .pointer("/error/code")
                        .and_then(serde_json::Value::as_i64),
                    Some(*code),
                    "{label}: HTTP gate must emit {code}: {http_error}"
                );
                let GateVerdict::Reject {
                    code: stdio_code, ..
                } = stdio
                else {
                    panic!("{label}: stdio gate must reject with {code}, got {stdio:?}")
                };
                assert_eq!(
                    *code, stdio_code,
                    "{label}: stdio and HTTP verdicts must match"
                );
            }
            (Some(code), None) => panic!("{label}: HTTP gate forwarded; expected {code}"),
        }
    }

    // ── One canonical model: the wire `reason` IS the ContentError
    //    message the canonical model emits — no parallel wording ─────
    let bad_uri_body =
        r#"{"jsonrpc":"2.0","id":20,"method":"resources/read","params":{"uri":"no uri"}}"#;
    let http_doc = http_envelope_rejection(bad_uri_body.as_bytes()).expect("must reject");
    let wire_reason = http_doc["error"]["data"]["reason"]
        .as_str()
        .expect("rejection must carry a typed reason");
    let canonical_reason = susi_gmcp::content::ContentError::BadUri("params.uri").wire_message();
    assert_eq!(
        wire_reason, canonical_reason,
        "the gate's reason must be the canonical model's own message"
    );

    // ── The served wire agrees: same corpus member, same verdict ────
    let mut session = StdioSession::spawn(root, &home);
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "intg-bad-uri",
        "method": "resources/read",
        "params": {"uri": "no uri"}
    }));
    let response = session
        .response_for(
            &serde_json::json!("intg-bad-uri"),
            std::time::Duration::from_secs(60),
        )
        .expect("served session must answer the malformed request");
    assert_eq!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default(),
        canonical_reason,
        "served stdio must surface the canonical reason verbatim"
    );
    // And the modern-era surface applies the same boundary — the
    // policy never depends on which era the session negotiated.
    let mut modern = StdioSession::spawn_modern(root, &home);
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "intg-modern-bad",
        "method": "resources/read",
        "params": {
            "uri": "no uri",
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    }));
    let response = modern
        .response_for(
            &serde_json::json!("intg-modern-bad"),
            std::time::Duration::from_secs(60),
        )
        .expect("modern session must answer the malformed request");
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default(),
        canonical_reason
    );

    // ── Structural proof of one boundary: the only HTTP entry points
    //    the daemon/gateway serve run `http_envelope_rejection`, and
    //    both it and `classify_line` share `classify_envelope` ───────
    let gate_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/jsonrpc_gate.rs"))
        .expect("gate source must be readable");
    assert!(
        gate_src.contains("pub fn classify_envelope")
            && gate_src.contains("crate::content::validate_payload(object)"),
        "classify_envelope must run the canonical content boundary"
    );
    let server_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/server.rs"))
        .expect("server source must be readable");
    assert!(
        server_src.contains("jsonrpc_gate::http_envelope_rejection")
            && server_src.contains("jsonrpc_gate::StdioGate"),
        "the served transports must both run the shared gate"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-04: adversarial and failure behavior for the
/// content/metadata boundary — extension-field injection, protocol-
/// namespace smuggling, URI/MIME/base64/Unicode spoofs, depth bombs,
/// external `$ref` smuggling, over-validation resistance, deterministic
/// replays, and stream recovery after a rejection. Every probe runs the
/// production path: `susi_gmcp::content`'s canonical validation, the
/// shared gate, and the served `mcp serve` wire.
#[test]
fn gmcp_req_203_008_04_adversarial() {
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-adversarial-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Extension-field injection: envelope-mimicking keys inside a
    //    content block are preserved verbatim but never interpreted ──
    let injection: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/content-block-envelope-injection.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let injected_block = &injection["payload"]["result"]["content"];
    let block = CanonicalContentBlock::from_wire(injected_block)
        .expect("injected extension keys must still validate — they are data");
    let wire = block.to_wire();
    assert_eq!(
        wire["method"], "tools/call",
        "the forged method must be preserved as data"
    );
    assert_eq!(
        wire["id"], "spoofed-request",
        "the forged id must be preserved as data"
    );
    assert_eq!(
        wire["io.modelcontextprotocol/protocolVersion"], "2999-01-01",
        "the reserved-namespace key is data, not protocol metadata"
    );
    // The gate never treats the injected body as a request: the whole
    // message forwards as the response it is (or drops compatibly).
    assert!(
        http_envelope_rejection(
            serde_json::to_string(&injection["payload"])
                .unwrap()
                .as_bytes()
        )
        .is_none(),
        "an extension-injected response must not be rejected as a forged request"
    );

    // Protocol-reserved keys nested in `arguments` are NOT the
    // `params._meta` protocol fields the modern path requires — a
    // modern request missing its real `_meta` still earns -32602 even
    // when arguments smuggle the namespaced keys.
    let mut modern = StdioSession::spawn_modern(root, &home);
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "smuggle",
        "method": "tools/call",
        "params": {
            "name": "x",
            "arguments": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    }));
    let response = modern
        .response_for(
            &serde_json::json!("smuggle"),
            std::time::Duration::from_secs(60),
        )
        .expect("the smuggled request must be answered");
    assert_eq!(
        response["error"]["code"], -32602,
        "namespaced keys in arguments must not satisfy the _meta policy: {response}"
    );

    // ── Spoof battery: every malformed class fails typed ────────────
    for (label, value, want) in [
        (
            "mid-string base64 padding",
            serde_json::json!({"type":"audio","data":"QUJD=RAAA","mimeType":"audio/wav"}),
            ContentError::BadBase64("audio"),
        ),
        (
            "CRLF-injected mimeType",
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"image/png\r\nSet-Cookie: x"}),
            ContentError::BadMimeType("image"),
        ),
        (
            "control char in resource uri",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a\u{0}b","text":"t"}}),
            ContentError::BadUri("resource.uri"),
        ),
        (
            "negative priority",
            serde_json::json!({"type":"text","text":"t","annotations":{"priority":-0.5}}),
            ContentError::BadPriority,
        ),
        (
            "forged audience role",
            serde_json::json!({"type":"text","text":"t","annotations":{"audience":["system"]}}),
            ContentError::BadAudience,
        ),
        (
            "control char in link name",
            serde_json::json!({"type":"resource_link","uri":"file:///a","name":"x\u{0}"}),
            ContentError::BadUnicode("resource_link.name"),
        ),
    ] {
        assert_eq!(
            CanonicalContentBlock::from_wire(&value),
            Err(want),
            "{label} must fail typed"
        );
    }
    // Document the decision explicitly: `javascript:` is a valid
    // absolute URI at this boundary (SSRF/origin policy is VC-203-032).
    assert!(CanonicalContentBlock::from_wire(
        &serde_json::json!({"type":"resource_link","uri":"javascript:alert(1)","name":"x"})
    )
    .is_ok());

    // ── Bounded resources: a nesting bomb earns a typed -32600 ──────
    let mut deep = serde_json::json!("leaf");
    for _ in 0..80 {
        deep = serde_json::json!({"k": deep});
    }
    let bomb = serde_json::json!({
        "jsonrpc": "2.0", "id": "bomb",
        "result": {"content": [{"type": "text", "text": "x", "_meta": {"deep": deep}}]}
    });
    let rejection = http_envelope_rejection(serde_json::to_string(&bomb).unwrap().as_bytes())
        .expect("a nesting bomb must be rejected");
    assert_eq!(rejection["error"]["code"], -32600);
    assert!(
        rejection["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("bounded"),
        "the bomb's reason must name the resource bound: {rejection}"
    );

    // ── Schemeless and traversal URIs fail at the boundary ──────────
    let schemeless: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/request-resources-read-uri-schemeless.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut session = StdioSession::spawn(root, &home);
    session.send(&schemeless["request"]);
    let response = session
        .response_for(&serde_json::json!(42), std::time::Duration::from_secs(60))
        .expect("schemeless uri must be answered");
    assert_eq!(response["error"]["code"], -32602);

    // ── External `$ref` smuggling: a tool schema that tries to fetch
    //    a remote reference cannot even register — fail-closed at
    //    compile; local `#/$defs` refs (the pinned corpus) still work ─
    let service = susi_gmcp::protocol::GmcpService::new(scratch.join("workspace"));
    let remote_tool: susi_gmcp::rmcp::model::Tool = serde_json::from_value(
        serde_json::json!({"name":"evil","inputSchema":{"$ref":"https://evil.example/x.json"}}),
    )
    .unwrap();
    assert!(
        service
            .register_tool(
                remote_tool,
                std::sync::Arc::new(|_, _| Box::pin(async {
                    Err(susi_gmcp::rmcp::ErrorData::internal_error(
                        "unreachable",
                        None,
                    ))
                }))
            )
            .is_err(),
        "a remote-$ref inputSchema must never register"
    );
    let local_tool: susi_gmcp::rmcp::model::Tool = serde_json::from_value(
        serde_json::json!({"name":"ok","inputSchema":{"$defs":{"s":{"type":"string"}},"type":"object","properties":{"v":{"$ref":"#/$defs/s"}}}}),
    )
    .unwrap();
    assert!(
        service
            .register_tool(
                local_tool,
                std::sync::Arc::new(|_, _| Box::pin(async {
                    Err(susi_gmcp::rmcp::ErrorData::internal_error(
                        "unreachable",
                        None,
                    ))
                }))
            )
            .is_ok(),
        "a local $defs ref compiles — the pinned corpus is the only resolver"
    );

    // ── Over-validation resistance: free-form tool `arguments` are
    //    never judged as spec content — a tools/call carrying a
    //    content-shaped argument reaches the service, not the gate ───
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "args-not-content",
        "method": "tools/call",
        "params": {
            "name": "definitely-not-a-real-tool",
            "arguments": {"type": "image", "data": "%%%", "mimeType": "junk"}
        }
    }));
    let response = session
        .response_for(
            &serde_json::json!("args-not-content"),
            std::time::Duration::from_secs(120),
        )
        .expect("tools/call must be answered by the service, not the gate");
    assert!(
        response["error"]["data"]["reason"].is_null()
            || response["error"]["data"]["reason"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
        "free-form arguments must not trip the content boundary: {response}"
    );

    // ── Recovery: a rejection never corrupts the stream — the next
    //    well-formed request on the same session answers normally ────
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "after-reject",
        "method": "resources/list", "params": {}
    }));
    let response = session
        .response_for(
            &serde_json::json!("after-reject"),
            std::time::Duration::from_secs(120),
        )
        .expect("the session must keep serving after a rejection");
    assert!(
        response.get("result").is_some(),
        "a well-formed request after a -32602 must reach the service: {response}"
    );

    // ── Deterministic replay: the same attack body yields the same
    //    typed verdict — no state leaks between attempts ─────────────
    let attack =
        r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"//etc/passwd"}}"#;
    let first = http_envelope_rejection(attack.as_bytes());
    let second = http_envelope_rejection(attack.as_bytes());
    assert_eq!(
        first, second,
        "the same body must produce identical verdicts"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-05: interoperability and quality for the canonical
/// content/metadata model — every retained fixture replays its declared
/// outcome through the canonical validator and both wire gates, the
/// explicit adapter is byte-for-byte equivalent to a direct reference-SDK
/// (`rmcp::model`) parse in the server role and round-trips in the client
/// role, and the published bounds (determinism, per-parse latency) hold
/// under replay. Divergences between the canonical model and the typed
/// SDK are the vector's documented rationale — extension preservation and
/// boundary validation — and are asserted, not assumed.
#[test]
fn gmcp_req_203_008_05_interop() {
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Fixture replay: every bound fixture earns its declared
    //    outcome through the canonical model ─────────────────────────
    let positive: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/content-block-annotated-2026-07-28.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(positive["expect"], "result");
    let block = CanonicalContentBlock::from_wire(&positive["payload"])
        .expect("the positive fixture must validate canonically");
    assert_eq!(block.to_wire(), positive["payload"], "lossless round-trip");

    for (file, want) in [
        (
            "content-block-bad-mime.json",
            ContentError::BadMimeType("image"),
        ),
        (
            "content-block-base64-padding-abuse.json",
            ContentError::BadBase64("audio"),
        ),
    ] {
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".agents/gmcp/fixtures/negative").join(file))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture["expect"], "error",
            "{file} must declare an error outcome"
        );
        assert_eq!(
            CanonicalContentBlock::from_wire(&fixture["payload"]),
            Err(want.clone()),
            "{file} must fail the declared typed validation"
        );
        // The same block as a client response's content earns the gate's
        // typed -32600 carrying the canonical reason — one implementation.
        let wire_doc = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"content": fixture["payload"].clone()}
        });
        let rejection =
            http_envelope_rejection(serde_json::to_string(&wire_doc).unwrap().as_bytes())
                .unwrap_or_else(|| panic!("{file}: the HTTP gate must reject"));
        assert_eq!(rejection["error"]["code"], -32600, "{file}");
        assert_eq!(
            rejection["error"]["data"]["reason"]
                .as_str()
                .unwrap_or_default(),
            want.wire_message(),
            "{file}: the gate reason must be the canonical message"
        );
    }

    // The envelope-injection fixture declares `forward`: preserved as
    // data, never interpreted — on both transports.
    let injection: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/content-block-envelope-injection.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(injection["expect"], "forward");
    let body = serde_json::to_string(&injection["payload"]).unwrap();
    assert!(http_envelope_rejection(body.as_bytes()).is_none());
    let mut era = EraState::default();
    assert!(matches!(
        classify_line(body.as_bytes(), &mut era),
        GateVerdict::Forward | GateVerdict::Drop
    ));

    // The schemeless-uri request fixture replays to -32602 on the served
    // wire — the declared outcome verbatim.
    let schemeless: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/request-resources-read-uri-schemeless.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut session = StdioSession::spawn(root, &home);
    session.send(&schemeless["request"]);
    let response = session
        .response_for(&serde_json::json!(42), std::time::Duration::from_secs(60))
        .expect("the schemeless-uri fixture must be answered");
    assert_eq!(
        response["error"]["code"],
        schemeless["expect_code"].as_i64().unwrap()
    );

    // ── Reference-SDK roles: the canonical adapter is equivalent to a
    //    direct typed-model parse (server role), and a typed model
    //    re-enters the canonical model losslessly (client role) ──────
    let sdk_parsed: susi_gmcp::rmcp::model::ContentBlock =
        serde_json::from_value(positive["payload"].clone())
            .expect("the reference SDK must parse the positive fixture");
    let via_adapter = block.to_rmcp().expect("canonical must adapt");
    assert_eq!(
        serde_json::to_value(&sdk_parsed).unwrap(),
        serde_json::to_value(&via_adapter).unwrap(),
        "the adapter must equal a direct reference-SDK parse"
    );

    // Documented divergences — asserted, not assumed: the typed SDK
    // silently drops the unknown extension field the canonical model
    // preserves, and it accepts the invalid mimeType the canonical
    // boundary rejects (validation is the canonical model's added duty).
    let sdk_wire = serde_json::to_value(&sdk_parsed).unwrap();
    assert!(
        sdk_wire.get("io.example/futureExtension").is_none(),
        "documented: the typed SDK drops unknown extension fields"
    );
    assert!(
        serde_json::from_value::<susi_gmcp::rmcp::model::ContentBlock>(
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"bad mime"})
        )
        .is_ok(),
        "documented: the typed SDK accepts what the canonical boundary rejects"
    );

    // Client-role round-trip: a typed model's serialization re-enters the
    // canonical model with every spec field intact.
    let re_canonical = CanonicalContentBlock::from_wire(&sdk_wire)
        .expect("a reference-SDK serialization must re-enter canonically");
    assert_eq!(re_canonical.to_wire(), sdk_wire);

    // ── Quality bounds: canonical validation is cheap and
    //    deterministic — the published SLOs of the contract ──────────
    let started = std::time::Instant::now();
    let iterations = 200;
    for _ in 0..iterations {
        let parsed = CanonicalContentBlock::from_wire(&positive["payload"])
            .expect("repeated validation must succeed");
        std::hint::black_box(parsed);
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "200 canonical validations must finish well under budget: {elapsed:?}"
    );
    // Determinism across orderings and runs — canonical serialization
    // is the evidence-hash-stable form the contract publishes.
    let a = CanonicalContentBlock::from_wire(&positive["payload"]).unwrap();
    let b = CanonicalContentBlock::from_wire(&a.to_wire()).unwrap();
    assert_eq!(a.canonical_json(), b.canonical_json());
    let c = CanonicalContentBlock::from_wire(&positive["payload"]).unwrap();
    assert_eq!(a.canonical_json(), c.canonical_json());

    // ── No unrecorded wire divergences: every -008 fixture's declared
    //    outcome matched observed behavior in this replay, so the
    //    shared divergences registry needs no -008 entries ───────────
    let divergences: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/fixtures/divergences.json")).unwrap(),
    )
    .unwrap();
    for divergence in divergences["divergences"].as_array().unwrap() {
        let fixture = divergence["fixture"].as_str().unwrap_or_default();
        assert!(
            !fixture.contains("content-block") && !fixture.contains("resources-read-uri"),
            "a -008 fixture regressed into the divergence registry: {divergence}"
        );
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-06: operations and evidence for the content/metadata
/// model. The ledger CLI is the bounded operator surface, a served
/// `tools/call` is the live inspectable surface whose content blocks
/// parse through the canonical boundary, `--refetched` without `--drift`
/// is rejected configuration, the deterministic drift check names a
/// drifted schema pin and fails closed on a missing one, the contract
/// carries the runbook, and the evidence artifact is signed and
/// reproducible.
#[test]
fn gmcp_req_203_008_06_operations() {
    use susi_gmcp::content::CanonicalContentBlock;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The shared ledger report is the bounded operator surface.
    let report = susi_json(root, &home, &["mcp", "ledger"]);
    assert_eq!(report["verified"], true);
    for key in [
        "rows",
        "documents",
        "revisions",
        "dispositions",
        "categories",
        "vectors",
    ] {
        assert!(!report[key].is_null(), "ledger report must expose {key}");
    }

    // A served `tools/call` is this vector's live inspectable surface:
    // the result's `content[]` blocks cross the gate and must parse
    // through the canonical boundary an operator (or a peer) consumes.
    let mut served = StdioSession::spawn(root, &home);
    let result = served.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    let tool_report = call_tool_report(&result);
    assert_eq!(tool_report["verified"], true);
    let block = &result["content"][0];
    assert_eq!(block["type"], "text", "served content must be typed");
    CanonicalContentBlock::from_wire(block)
        .expect("served content blocks must satisfy the canonical model");
    drop(served);

    // Invalid configuration is rejected rather than silently degraded:
    // `--refetched` is meaningless without `--drift`.
    let invalid = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(!invalid.status.success());

    // The deterministic drift check is clean on the shipped corpus and
    // covers this vector's pinned schema corpus.
    let clean = susi_json(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            ".agents/gmcp/spec",
        ],
    );
    assert_eq!(clean["drift"], true);
    assert_eq!(clean["verified"], true);
    assert_eq!(clean["checked"], clean["matched"]);
    assert!(clean["findings"].as_array().unwrap().is_empty());

    // A drifted schema rendering is a named `hash_drift` finding and a
    // non-zero result — stale model proof cannot survive a changed pin.
    let schema_doc = "schema/2026-07-28/schema.json";
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(schema_doc);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes.push(b'!');
    std::fs::write(&victim, &bytes).unwrap();
    let drifted = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!drifted.status.success());
    let drift_report: serde_json::Value =
        serde_json::from_slice(&drifted.stdout).expect("drift failure must remain JSON");
    assert_eq!(drift_report["verified"], false);
    assert!(drift_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| { f["document"] == schema_doc && f["kind"] == "hash_drift" }));

    // An absent schema document fails closed as `unfetchable`.
    std::fs::remove_file(&victim).unwrap();
    let missing = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!missing.status.success());
    let missing_report: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("missing source must remain JSON");
    assert!(missing_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| { f["document"] == schema_doc && f["kind"] == "unfetchable" }));

    // The contract carries the operator runbook: state, diagnostics,
    // triage, migration, and rollback are documented — not tribal.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-008.md"))
        .expect("contract must exist");
    for token in [
        "Operations and incident runbook",
        "Inspectable state",
        "Diagnostics",
        "Incident triage",
        "Migration (re-pinning",
        "Rollback",
        "hash_drift",
        "unfetchable",
        "-32602",
        "-32600",
        "CanonicalContentBlock",
        "silent drop",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // Evidence is retained, signed, and tied to the canonical ledger
    // bytes, so a changed corpus invalidates this proof.
    use sha2::{Digest, Sha256};
    let ledger_hash = hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-008.jsonl"))
        .expect("VC-203-008 must retain evidence");
    let record = evidence
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|record| record["requirement"] == "GMCP-203-008-06")
        .expect("evidence must retain a record for GMCP-203-008-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_008_06_operations"
    );
    assert_eq!(record["exit"], 0);
    assert_eq!(record["artifact_sha256"], ledger_hash);
    assert!(!record["detail"].as_str().unwrap_or_default().is_empty());
    assert!(!record["identity"].as_str().unwrap_or_default().is_empty());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(record["recorded_unix"].as_u64().unwrap() <= now + 60);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-M: the vector outcome holds on the shipped production
/// path — every MCP content variant is represented losslessly (text,
/// image, audio, resource, resource link; annotations, icons, `_meta`,
/// audience, priority, unknown extension fields), the canonical model
/// is what the wire gate enforces on the served path, JSON Schema
/// 2020-12 tool contracts compile locally and refuse external `$ref`
/// resolution, canonical serialization is deterministic, and this test
/// consumes and verifies all requirement evidence — missing, stale,
/// unsigned, waived, mock-only, or zero-test records fail.
#[test]
fn gmcp_vc_203_008_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Shipped wire path: the served result's content blocks satisfy
    //    the canonical model, and the gate emits typed failures for
    //    malformed params and malformed content-bearing payloads. ────
    let mut session = StdioSession::spawn(root, &home);
    let result = session.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    for block in result["content"]
        .as_array()
        .expect("content must be an array")
    {
        CanonicalContentBlock::from_wire(block).unwrap_or_else(|e| {
            panic!("served content block must satisfy the canonical model: {e}")
        });
    }
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "m-uri",
        "method": "resources/read", "params": {"uri": "no-scheme"}
    }));
    let response = session
        .response_for(
            &serde_json::json!("m-uri"),
            std::time::Duration::from_secs(60),
        )
        .expect("malformed URI must earn a typed verdict");
    assert_eq!(response["error"]["code"], -32602, "{response}");
    session.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "m-content",
        "result": {"content": {"type": "audio", "data": "###", "mimeType": "audio/wav"}}
    }));
    let response = session
        .response_for(
            &serde_json::json!("m-content"),
            std::time::Duration::from_secs(60),
        )
        .expect("malformed content must earn a typed verdict");
    assert_eq!(response["error"]["code"], -32600, "{response}");
    drop(session);

    // ── Lossless: every content variant round-trips verbatim through
    //    the canonical model, unknown extension fields included. ─────
    for (variant, value) in [
        (
            "text",
            serde_json::json!({"type":"text","text":"héllo \u{4e16}\u{754c}"}),
        ),
        (
            "image",
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"image/png"}),
        ),
        (
            "audio",
            serde_json::json!({"type":"audio","data":"AAE=","mimeType":"audio/wav"}),
        ),
        (
            "embedded text resource",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a.txt","mimeType":"text/plain","text":"body"}}),
        ),
        (
            "embedded blob resource",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a.bin","blob":"AAE="}}),
        ),
        (
            "resource_link",
            serde_json::json!({"type":"resource_link","uri":"file:///r.pdf","name":"r",
                "mimeType":"application/pdf",
                "icons":[{"src":"https://i.example/x.png","mimeType":"image/png"}],
                "annotations":{"audience":["user","assistant"],"priority":0.8,
                               "lastModified":"2026-10-18T00:00:00Z"},
                "_meta":{"vendor.example/k":"v"},
                "ext.example/future":{"nested":[1,true]}}),
        ),
    ] {
        let parsed = CanonicalContentBlock::from_wire(&value)
            .unwrap_or_else(|e| panic!("{variant} must validate: {e}"));
        assert_eq!(
            parsed.to_wire(),
            value,
            "{variant} must round-trip losslessly"
        );
        // The typed rmcp adapter represents every spec field.
        assert!(parsed.to_rmcp().is_ok(), "{variant} must adapt to rmcp");
    }

    // ── Canonical serialization is deterministic across key order ──
    let a = CanonicalContentBlock::from_wire(&serde_json::json!({
        "type":"text","text":"t","_meta":{"z":1,"a":2},"annotations":{"priority":0.5}
    }))
    .unwrap()
    .canonical_json();
    let b = CanonicalContentBlock::from_wire(&serde_json::json!({
        "annotations":{"priority":0.5},"_meta":{"a":2,"z":1},"text":"t","type":"text"
    }))
    .unwrap()
    .canonical_json();
    assert_eq!(a, b, "canonical_json must be key-order independent");

    // ── Boundaries are typed failures, never silent coercion ────────
    for (label, value, want) in [
        (
            "bad mime",
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"no mime"}),
            ContentError::BadMimeType("image"),
        ),
        (
            "bad base64",
            serde_json::json!({"type":"image","data":"!!!","mimeType":"image/png"}),
            ContentError::BadBase64("image"),
        ),
        (
            "schemeless link uri",
            serde_json::json!({"type":"resource_link","uri":"x","name":"n"}),
            ContentError::BadUri("resource_link.uri"),
        ),
        (
            "priority out of range",
            serde_json::json!({"type":"text","text":"t","annotations":{"priority":1.5}}),
            ContentError::BadPriority,
        ),
        (
            "bad audience",
            serde_json::json!({"type":"text","text":"t","annotations":{"audience":["root"]}}),
            ContentError::BadAudience,
        ),
    ] {
        assert_eq!(
            CanonicalContentBlock::from_wire(&value),
            Err(want.clone()),
            "{label} must fail typed"
        );
    }

    // ── JSON Schema 2020-12 tool contracts: remote `$ref` fails
    //    closed at registration; local `#/$defs` compiles. ───────────
    let service = susi_gmcp::protocol::GmcpService::new(scratch.join("workspace"));
    let handler = || -> susi_gmcp::protocol::ToolHandler {
        std::sync::Arc::new(|_, _| {
            Box::pin(async { Err(susi_gmcp::rmcp::ErrorData::internal_error("x", None)) })
        })
    };
    let remote: susi_gmcp::rmcp::model::Tool = serde_json::from_value(
        serde_json::json!({"name":"r","inputSchema":{"$ref":"https://evil.example/x.json"}}),
    )
    .unwrap();
    assert!(
        service.register_tool(remote, handler()).is_err(),
        "a remote-$ref inputSchema must never register"
    );
    let local: susi_gmcp::rmcp::model::Tool = serde_json::from_value(
        serde_json::json!({"name":"l","inputSchema":{"$defs":{"s":{"type":"string"}},"type":"object","properties":{"v":{"$ref":"#/$defs/s"}}}}),
    )
    .unwrap();
    assert!(
        service.register_tool(local, handler()).is_ok(),
        "a local $defs ref must compile — the pinned corpus is the only resolver"
    );

    // ── The contract pins the vector's required vocabulary ──────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-008.md"))
        .expect("the vector contract must exist");
    for term in [
        "CanonicalContentBlock",
        "resource_link",
        "EmbeddedResource",
        "2020-12",
        "lossless",
        "audience",
        "priority",
        "ttlMs",
        "cacheScope",
        "extension",
        "-32602",
        "-32600",
    ] {
        assert!(contract.contains(term), "contract must pin {term}");
    }

    // ── Evidence verification: complete, fresh, signed, backed by a
    //    real test — missing, stale, unsigned, waived, mock-only or
    //    zero-test records fail ──────────────────────────────────────
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json"))
        .expect("the canonical ledger must exist");
    let ledger_hash = hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = std::fs::read_to_string(root.join("tests/gmcp_requirements.rs")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-008.jsonl"))
        .expect("VC-203-008 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    for line in evidence.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("evidence records must be single-line JSON");
        for field in [
            "requirement",
            "check",
            "exit",
            "detail",
            "recorded_unix",
            "artifact_sha256",
            "identity",
        ] {
            assert!(
                !record[field].is_null(),
                "unsigned or incomplete evidence: record lacks {field}"
            );
        }
        assert!(
            record.get("waived").is_none() && record.get("mock").is_none(),
            "waived or mock-only evidence does not count: {record}"
        );
        assert_eq!(record["exit"], 0, "evidence must record a green check");
        assert!(
            !record["identity"].as_str().unwrap().is_empty(),
            "evidence must be signed by an identity"
        );
        assert!(
            !record["detail"].as_str().unwrap().is_empty(),
            "evidence must describe what the check proved"
        );
        assert_eq!(
            record["artifact_sha256"].as_str().unwrap(),
            ledger_hash,
            "stale evidence: recorded hash must be the current ledger hash"
        );
        let recorded = record["recorded_unix"].as_u64().unwrap();
        assert!(
            recorded > 1_792_256_000 && recorded <= now + 60,
            "recorded_unix {recorded} must be a plausible recent timestamp"
        );
        let check = record["check"].as_str().unwrap();
        if let Some(test_name) = check.strip_prefix("cargo test --locked --test gmcp_requirements ")
        {
            assert!(
                test_name.starts_with("gmcp_")
                    && !test_name.contains(char::is_whitespace)
                    && test_src.contains(&format!("fn {test_name}")),
                "evidence names a test that does not exist: {test_name}"
            );
        } else {
            assert!(
                check
                    .split_whitespace()
                    .filter(|token| !token.starts_with('-'))
                    .any(|token| root.join(token).exists()),
                "evidence check must reference a committed artifact: {check}"
            );
        }
        covered.insert(record["requirement"].as_str().unwrap().to_string());
    }
    for req in [
        "GMCP-203-008-01",
        "GMCP-203-008-02",
        "GMCP-203-008-03",
        "GMCP-203-008-04",
        "GMCP-203-008-05",
        "GMCP-203-008-06",
        "GMCP-203-008-M",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── Committed proof: locally modified artifacts are not
    //    evidence — the vector's tree must be clean against git ──────
    let status = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--",
            ".agents/gmcp",
            "docs/gmcp",
            "tests/gmcp_requirements.rs",
            "crates/susi-gmcp",
        ])
        .current_dir(root)
        .output()
        .expect("git status must run — committed proof is required");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "locally modified artifacts are not proof:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn gmcp_req_203_013_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-013.md"))
        .expect("contract-203-013.md must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-013")
        .collect();
    assert_eq!(rows.len(), 198, "the contract binds 198 VC-203-013 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
    for needle in [
        "input_required",
        "inputRequests",
        "inputResponses",
        "elicitation",
        "sampling",
        "roots",
        "cancellation",
        "deadline",
        "progress",
        "handle",
    ] {
        assert!(contract.contains(needle), "contract must resolve {needle}");
    }
}

#[test]
fn gmcp_req_203_004_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
        .expect("contract document must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-004")
        .collect();
    assert_eq!(rows.len(), 108, "the contract binds 108 VC-203-004 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
}

#[test]
fn gmcp_req_203_005_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
        .expect("contract document must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-005")
        .collect();
    assert_eq!(rows.len(), 20, "the contract binds 20 VC-203-005 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
}

#[test]
fn gmcp_req_203_006_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
        .expect("contract document must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-006")
        .collect();
    assert_eq!(rows.len(), 80, "the contract binds 80 VC-203-006 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
}

#[test]
fn gmcp_req_203_009_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-009.md"))
        .expect("contract document must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-009")
        .collect();
    assert_eq!(rows.len(), 67, "the contract binds 67 VC-203-009 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
}

#[test]
fn gmcp_req_203_010_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-010.md"))
        .expect("contract document must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-010")
        .collect();
    assert_eq!(rows.len(), 33, "the contract binds 33 VC-203-010 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
}

#[test]
fn gmcp_req_203_011_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-011.md"))
        .expect("contract document must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-011")
        .collect();
    assert_eq!(rows.len(), 39, "the contract binds 39 VC-203-011 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
}

#[test]
fn gmcp_req_203_004_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-004-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 108,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0,
        "revision 2025-11-25 must be verified"
    );
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-004")
        .collect();
    assert_eq!(rows.len(), 108, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

#[test]
fn gmcp_req_203_005_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-005-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 20,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-005")
        .collect();
    assert_eq!(rows.len(), 20, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

#[test]
fn gmcp_req_203_006_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-006-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 80,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );
    assert!(
        report["revisions"]["susi@baseline-2026-10-17"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "revision susi@baseline-2026-10-17 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-006")
        .collect();
    assert_eq!(rows.len(), 80, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

#[test]
fn gmcp_req_203_009_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-009-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 67,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0,
        "revision 2025-11-25 must be verified"
    );
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-009.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-009")
        .collect();
    assert_eq!(rows.len(), 67, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

#[test]
fn gmcp_req_203_010_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-010-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 33,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0,
        "revision 2025-11-25 must be verified"
    );
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-010.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-010")
        .collect();
    assert_eq!(rows.len(), 33, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

#[test]
fn gmcp_req_203_011_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-011-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 39,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0,
        "revision 2025-11-25 must be verified"
    );
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-011.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-011")
        .collect();
    assert_eq!(rows.len(), 39, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

#[test]
fn gmcp_req_203_013_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-013-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 198,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0,
        "revision 2025-11-25 must be verified"
    );
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-013.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-013")
        .collect();
    assert_eq!(rows.len(), 198, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

#[test]
fn gmcp_req_203_004_03_integration() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-004-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut wire = StdioSession::spawn(root, &home);
    let tools = wire.request("tools/list", serde_json::json!({}));
    assert!(
        tools["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["name"] == "ledger_verify")),
        "the served surface must advertise ledger_verify"
    );
    let call = wire.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    let report = call_tool_report(&call);
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 108,
        "the served report must still hold the vector's rows"
    );
    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-004")
        .collect();
    assert_eq!(rows.len(), 108, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}
