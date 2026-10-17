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
            other => panic!("{line} must reject, got {other:?}"),
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
        .response_for(&serde_json::json!(1), std::time::Duration::from_secs(30))
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
