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
    let mut child = susi_command(root, &home, &["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the susi binary must spawn `mcp serve`");
    let mut stdin = child.stdin.take().expect("stdio server must accept stdin");
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
    stdin
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"gmcp-it","version":"1.0"}}}"#
                .as_slice(),
        )
        .and_then(|()| {
            stdin.write_all(b"\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        })
        .expect("handshake must write");
    stdin.flush().expect("requests must flush");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut next_id = 2u64;
    let mut request = |params: serde_json::Value| {
        let id = next_id;
        next_id += 1;
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": params["method"], "params": params["params"]})
        )
        .expect("request must write");
        stdin.flush().expect("request must flush");
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                remaining > std::time::Duration::ZERO,
                "stdio server timed out"
            );
            let value = rx
                .recv_timeout(remaining)
                .expect("stdio server must answer");
            if value["id"].as_u64() == Some(id) {
                assert!(
                    value.get("error").is_none_or(|e| e.is_null()),
                    "JSON-RPC error: {value}"
                );
                return value["result"].clone();
            }
        }
    };

    // tools/list must advertise ledger_verify through the shared registry.
    let mut cursor = serde_json::Value::Null;
    let mut listed = false;
    for _ in 0..16 {
        let result = request(serde_json::json!({
            "method": "tools/list",
            "params": (if cursor.is_null() {
                serde_json::json!({})
            } else {
                serde_json::json!({"cursor": cursor})
            })
        }));
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
    let result = request(serde_json::json!({
        "method": "tools/call",
        "params": {"name": "ledger_verify", "arguments": {}}
    }));
    let wire_report = call_tool_report(&result);
    assert_eq!(
        wire_report, cli_report,
        "served tools/call must return the canonical report"
    );

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
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
