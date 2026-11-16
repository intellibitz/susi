//! Opt-in Linux preservation experiment, with a protocol-clean real child.
//! `cargo test --offline --locked -p susi --test harness_worker_mcp_parity -- --run`
//! This custom harness reports assertions, not libtest test counts. Ordinary
//! invocations skip it. Both sides call the existing production tool registry.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use susi_harness::tools::{Policy, Registry};
use susi_harness::workload_process::{self, Tree};
use susi_paths::http_body::read_line_capped;
use susi_wire_client::host::{DuplexStreams, HostPolicy, McpHost};

const MARKER: &str = "susi native-tool MCP preservation fixture v1";
const SYNTHETIC_KEY: &str = "sk-parity-synthetic-00000000000000000000000000000000";
const MAX_FRAME: usize = 64 * 1024;
const PEER_LIMIT: Duration = Duration::from_secs(10);

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/worker-mcp-parity")
}

fn owned(root: &Path) {
    assert!(root.is_absolute());
    assert!(root
        .canonicalize()
        .unwrap()
        .starts_with(fixture_root().canonicalize().unwrap()));
    assert_eq!(
        fs::read_to_string(root.join(".fixture-owned")).unwrap(),
        MARKER
    );
}

fn prepare(root: &Path) {
    fs::create_dir_all(root.join("workspace")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    fs::write(root.join(".fixture-owned"), MARKER).unwrap();
    owned(root);
}

fn command(root: &Path, args: &[&str]) -> Command {
    owned(root);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.env_clear().args(args).arg(root).current_dir(root);
    susi_paths::test_env::isolate_home(&mut command, &root.join("home"));
    command
        .env("SUSI_HOME", root.join("home/.susi"))
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("OPENAI_API_KEY", SYNTHETIC_KEY)
        .env("PATH", "/usr/bin:/bin")
        .env("CARGO_NET_OFFLINE", "true")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .stdin(Stdio::null());
    command
}

struct OwnedChild {
    child: Child,
    tree: Option<Tree>,
    status: Option<std::process::ExitStatus>,
}

impl OwnedChild {
    fn finish(&mut self) {
        // Drop the process tree before reaping its leader: no reused PID can
        // accidentally become the target of delayed group cleanup.
        drop(self.tree.take());
        if self.status.is_none() {
            let _ = self.child.kill();
            self.status = Some(self.child.wait().unwrap());
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.finish();
    }
}

struct Lease {
    owner: Arc<Mutex<OwnedChild>>,
    stop: Option<mpsc::Sender<()>>,
    watcher: Option<std::thread::JoinHandle<()>>,
}

impl Lease {
    fn new(child: Child, tree: Tree, limit: Duration) -> Self {
        let owner = Arc::new(Mutex::new(OwnedChild {
            child,
            tree: Some(tree),
            status: None,
        }));
        let watched = Arc::clone(&owner);
        let (stop, wait) = mpsc::channel();
        let watcher = std::thread::spawn(move || {
            if wait.recv_timeout(limit).is_err() {
                watched.lock().unwrap().finish();
            }
        });
        Self {
            owner,
            stop: Some(stop),
            watcher: Some(watcher),
        }
    }

    fn close(&mut self) {
        self.owner.lock().unwrap().finish();
        self.stop.take().unwrap().send(()).ok();
        self.watcher.take().unwrap().join().unwrap();
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.stop.is_some() {
            self.close();
        }
    }
}

fn registry(mode: &str) -> (Registry, Vec<String>) {
    let mut registry = Registry::builtin();
    let mut grants = Vec::new();
    if mode != "auto" {
        let mut spec = registry.get("write_to_file").unwrap().clone();
        spec.policy = match mode {
            "deny" | "deny-granted" => Policy::Deny,
            "ask" | "ask-granted" => Policy::Ask,
            _ => panic!("invalid fixed fixture policy"),
        };
        registry.register(spec);
        if mode.ends_with("-granted") {
            grants.push("write_to_file".into());
        }
    }
    (registry, grants)
}

fn catalog(registry: &Registry) -> Vec<Value> {
    registry
        .tools()
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name, "description": tool.description, "inputSchema": tool.parameters
            })
        })
        .collect()
}

fn write_frame(writer: &mut impl Write, frame: &Value) {
    let bytes = serde_json::to_vec(frame).unwrap();
    assert!(bytes.len() < MAX_FRAME);
    writer.write_all(&bytes).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
}

fn read_frame(reader: &mut impl io::BufRead) -> Option<Value> {
    let mut line = Vec::new();
    read_line_capped(reader, &mut line, MAX_FRAME as u64).unwrap();
    if line.is_empty() {
        return None;
    }
    assert!(line.ends_with(b"\n"), "incomplete fixture response");
    Some(serde_json::from_slice(&line).unwrap())
}

fn append_call(root: &Path, call: &Value) {
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("calls.jsonl"))
        .unwrap();
    writeln!(log, "{call}").unwrap();
    log.flush().unwrap();
}

fn call_count(root: &Path) -> usize {
    fs::read_to_string(root.join("calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .count()
}

fn server(root: &Path, mode: &str, behavior: &str) {
    owned(root);
    // If the suite itself is forcibly stopped, its in-process leases are
    // gone. Bound this fixture server independently as well. The only child
    // it can start is the separately bounded descendant fixture below.
    let (_stop, wait) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if matches!(
            wait.recv_timeout(Duration::from_secs(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            std::process::exit(124);
        }
    });
    assert!(matches!(behavior, "normal" | "lost"));
    let (registry, grants) = registry(mode);
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = io::stdout().lock();
    let mut negotiated = false;
    let mut initialized = false;
    loop {
        let mut line = Vec::new();
        read_line_capped(&mut input, &mut line, MAX_FRAME as u64).unwrap();
        if line.is_empty() {
            return;
        }
        assert!(line.ends_with(b"\n"), "incomplete fixture request");
        let request: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) => {
                write_frame(
                    &mut output,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                );
                continue;
            }
        };
        if !request.is_object()
            || request["jsonrpc"] != "2.0"
            || !request.get("method").is_some_and(Value::is_string)
            || request
                .get("id")
                .is_some_and(|id| !id.is_string() && !id.is_number())
        {
            write_frame(
                &mut output,
                &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}}),
            );
            continue;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        if method == "notifications/initialized" && id.is_null() && negotiated {
            initialized = true;
            continue;
        }
        if id.is_null() {
            continue;
        }
        let params = &request["params"];
        let result = match method {
            "initialize"
                if params["protocolVersion"].is_string()
                    && params["capabilities"].is_object()
                    && params["clientInfo"]["name"].is_string()
                    && params["clientInfo"]["version"].is_string() =>
            {
                negotiated = true;
                Ok(
                    json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"native-registry-fixture","version":"1"}}),
                )
            }
            "initialize" => Err((-32602, "Invalid initialize params")),
            "ping" => Ok(json!({})),
            "tools/list" if initialized => Ok(json!({"tools":catalog(&registry)})),
            "tools/call" if initialized => {
                let name = params.get("name").and_then(Value::as_str);
                let empty_args = json!({});
                let args = params.get("arguments").or(Some(&empty_args));
                match (name, args.filter(|args| args.is_object())) {
                    (Some(name), Some(_)) if registry.get(name).is_none() => {
                        Err((-32602, "Unknown tool"))
                    }
                    (Some(name), Some(args)) => {
                        append_call(root, &json!({"name":name,"arguments":args}));
                        let text = registry.execute(
                            &root.join("workspace"),
                            name,
                            &args.to_string(),
                            &grants,
                        );
                        if behavior == "lost" {
                            assert_eq!(name, "append_to_file");
                            // A real mutation completed. Keep the pipe open without a
                            // reply; the parent's lease must stop this entire tree.
                            let mut descendant = command(root, &["--descendant"])
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .spawn()
                                .unwrap();
                            let ready = Instant::now() + Duration::from_secs(2);
                            while !root.join("descendant.ready").exists() {
                                assert!(Instant::now() < ready);
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            let _ = descendant.wait();
                            return;
                        }
                        // Native handlers have a String contract, including errors.
                        // Keep that text intact; do not invent a typed error mapping.
                        Ok(json!({"content":[{"type":"text","text":text}],"isError":false}))
                    }
                    _ => Err((-32602, "Invalid params: name and arguments object required")),
                }
            }
            "tools/list" | "tools/call" => Err((-32000, "Not initialized")),
            _ => Err((-32601, "Method not found")),
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err((code, message)) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
            }
        };
        write_frame(&mut output, &response);
    }
}

struct Peer {
    host: McpHost,
    lease: Lease,
    policy: Registry,
    grants: Vec<String>,
}

impl Peer {
    fn open(root: &Path, mode: &str, behavior: &str, limit: Duration) -> Self {
        let (mut child, tree) = workload_process::spawn(
            command(root, &["--server", mode, behavior])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(fs::File::create(root.join("server.stderr")).unwrap()),
        )
        .unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let writer = child.stdin.take().unwrap();
        let lease = Lease::new(child, tree, limit);
        let close = Arc::clone(&lease.owner);
        let mut host = McpHost::new();
        host.connect_duplex(
            "fixture",
            DuplexStreams {
                reader: Box::new(reader),
                writer: Box::new(writer),
                on_close: Some(Box::new(move || close.lock().unwrap().finish())),
            },
            HostPolicy::strict("harness-worker-parity", "1"),
        )
        .unwrap();
        let (policy, grants) = registry(mode);
        Self {
            host,
            lease,
            policy,
            grants,
        }
    }

    fn call(&mut self, name: &str, args: &Value) -> String {
        // Preserve native authority before transport; refused operations do
        // not become an unknown or unauthorized standard MCP invocation.
        if let Err(refusal) = self.policy.authorize(name, &self.grants) {
            return refusal;
        }
        let result = self.host.call_tool("fixture", name, args, None).unwrap();
        assert_eq!(result["isError"], false, "native errors remain text");
        assert_eq!(result["content"].as_array().unwrap().len(), 1);
        assert_eq!(result["content"][0]["type"], "text");
        result["content"][0]["text"].as_str().unwrap().to_owned()
    }

    fn close(&mut self) {
        self.host.disconnect("fixture").unwrap();
        self.lease.close();
        assert!(self.lease.owner.lock().unwrap().status.is_some());
    }
}

fn seed(root: &Path) {
    prepare(root);
    let ws = root.join("workspace");
    fs::write(ws.join("source.txt"), "α first\nsecond\nthird\n").unwrap();
    fs::write(ws.join("edit.txt"), "keep before\nanswer=41\nkeep after\n").unwrap();
    fs::write(ws.join("duplicate.txt"), "x x").unwrap();
    fs::write(ws.join("binary.bin"), [0xff, b'x']).unwrap();
    fs::write(ws.join("empty.txt"), []).unwrap();
    fs::create_dir_all(ws.join("empty-dir")).unwrap();
}

fn bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, path: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let name = path
                .strip_prefix(base)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if entry.file_type().unwrap().is_dir() {
                files.insert(format!("{name}/"), Vec::new());
                walk(base, &path, files);
            } else {
                files.insert(name, fs::read(path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

fn compare_objects(root: &Path, rows: &mut Vec<Value>) {
    let direct = root.join("direct");
    let remote = root.join("remote");
    seed(&direct);
    seed(&remote);
    let (native, grants) = registry("auto");
    let mut peer = Peer::open(&remote, "auto", "normal", PEER_LIMIT);
    let expected = catalog(&native);
    let limit = expected
        .iter()
        .map(|tool| serde_json::to_vec(tool).unwrap().len())
        .sum();
    assert_eq!(
        peer.host.list_tools_bounded("fixture", 6, limit).unwrap(),
        expected
    );
    let declarations = native.declarations();
    for (definition, tool) in declarations.as_array().unwrap().iter().zip(&expected) {
        assert_eq!(definition["function"]["name"], tool["name"]);
        assert_eq!(definition["function"]["description"], tool["description"]);
        assert_eq!(definition["function"]["parameters"], tool["inputSchema"]);
    }
    let mut cases = vec![
        ("view_file", json!({"path":"source.txt"})),
        (
            "view_file",
            json!({"path":"source.txt","start_line":2,"end_line":2}),
        ),
        ("view_file", json!({"path":"source.txt","start_line":100})),
        ("view_file", json!({"path":"empty.txt"})),
        ("view_file", json!({"path":"binary.bin"})),
        ("view_file", json!({"path":"missing"})),
        ("view_file", json!({"path":"../outside"})),
        ("view_file", json!({"path":"/outside"})),
        (
            "write_to_file",
            json!({"path":"nested/new.txt","content":"one"}),
        ),
        (
            "write_to_file",
            json!({"path":"nested/new.txt","content":"must not overwrite"}),
        ),
        (
            "append_to_file",
            json!({"path":"nested/new.txt","content":"two\n"}),
        ),
        (
            "append_to_file",
            json!({"path":"nested/new.txt","content":"three"}),
        ),
        (
            "append_to_file",
            json!({"path":"new-append.txt","content":"new ☃"}),
        ),
        (
            "append_to_file",
            json!({"path":"binary.bin","content":"tail"}),
        ),
        (
            "replace_file_content",
            json!({"path":"edit.txt","target":"absent","replacement":"oops"}),
        ),
        (
            "replace_file_content",
            json!({"path":"duplicate.txt","target":"x","replacement":"oops"}),
        ),
        (
            "replace_file_content",
            json!({"path":"edit.txt","target":"41","replacement":"42"}),
        ),
        ("list_dir", json!({})),
        ("list_dir", json!({"path":" empty-dir "})),
        ("list_dir", json!({"path":"missing"})),
        (
            "run_command",
            json!({"command":"printf 'alpha'; printf 'problem' >&2; exit 7"}),
        ),
        (
            "run_command",
            json!({"command":"printf '%s' \"${SUSI_WORKER_GUARD-unset}\""}),
        ),
        (
            "run_command",
            json!({"command":format!("printf '%s\\n' \"${{OPENAI_API_KEY-unset}}\"; printf '{SYNTHETIC_KEY}'")}),
        ),
        ("run_command", json!({"command":"susi workflow finish"})),
        ("unknown_tool", json!({})),
    ];
    for name in native.tools().iter().map(|tool| tool.name.as_str()) {
        cases.push((name, json!({})));
        cases.push((
            name,
            json!({"path":3,"content":false,"target":[],"replacement":null,"command":7}),
        ));
    }
    for (name, args) in cases {
        let before = bytes(&direct.join("workspace"));
        let direct_text =
            native.execute(&direct.join("workspace"), name, &args.to_string(), &grants);
        let calls_before = call_count(&remote);
        let remote_text = peer.call(name, &args);
        let transported = native.authorize(name, &grants).is_ok();
        assert_eq!(call_count(&remote), calls_before + usize::from(transported));
        assert_eq!(remote_text, direct_text, "{name} {args}");
        assert_eq!(
            bytes(&direct.join("workspace")),
            bytes(&remote.join("workspace")),
            "{name} {args}"
        );
        if direct_text.starts_with("refused:") || direct_text.starts_with("target ") {
            assert_eq!(bytes(&direct.join("workspace")), before);
        }
        if name == "run_command"
            && args["command"]
                .as_str()
                .is_some_and(|s| s.contains(SYNTHETIC_KEY))
        {
            assert!(direct_text.contains("unset"));
            assert!(!direct_text.contains(SYNTHETIC_KEY));
        }
        rows.push(json!({"name":name,"arguments":args,"native_text":direct_text,"adapter_text":remote_text,"transported":transported,"authority_checked_locally":true,"files_equal":true}));
    }
    assert_eq!(
        fs::read_to_string(direct.join("workspace/edit.txt")).unwrap(),
        "keep before\nanswer=42\nkeep after\n"
    );
    assert_eq!(
        fs::read_to_string(direct.join("workspace/nested/new.txt")).unwrap(),
        "one\ntwo\nthree"
    );
    assert_eq!(
        fs::read(direct.join("workspace/binary.bin")).unwrap(),
        [0xff, b'x', b'\n', b't', b'a', b'i', b'l']
    );
    assert!(rows.iter().any(|row| row["native_text"] == ""));
    peer.close();
}

fn policies_and_raw(root: &Path, rows: &mut Vec<Value>, raw: &mut Vec<Value>) {
    for mode in ["auto", "ask", "ask-granted", "deny", "deny-granted"] {
        let direct = root.join(mode).join("direct");
        let remote = root.join(mode).join("remote");
        seed(&direct);
        seed(&remote);
        let (native, grants) = registry(mode);
        let mut peer = Peer::open(&remote, mode, "normal", PEER_LIMIT);
        for name in ["write_to_file", "unknown_tool"] {
            for args in [json!({}), json!({"path":"policy.txt","content":"allowed"})] {
                let local =
                    native.execute(&direct.join("workspace"), name, &args.to_string(), &grants);
                let calls_before = call_count(&remote);
                let wire = peer.call(name, &args);
                let transported = native.authorize(name, &grants).is_ok();
                assert_eq!(call_count(&remote), calls_before + usize::from(transported));
                assert_eq!(wire, local);
                assert_eq!(
                    bytes(&direct.join("workspace")),
                    bytes(&remote.join("workspace"))
                );
                rows.push(json!({"policy":mode,"name":name,"arguments":args,"native_text":local,"adapter_text":wire,"transported":transported,"authority_checked_locally":true}));
            }
            let text = native.execute(&direct.join("workspace"), name, "{malformed", &grants);
            if name == "unknown_tool" {
                assert_eq!(text, "unknown tool 'unknown_tool'");
            } else if mode.starts_with("deny") {
                assert_eq!(text, "tool 'write_to_file' is denied by policy");
            } else if mode == "ask" {
                assert_eq!(text, "tool 'write_to_file' needs operator approval; grant it or add it to the run's approvals");
            } else {
                assert!(text.starts_with("error parsing tool arguments:"));
            }
            raw.push(json!({"policy":mode,"name":name,"raw":"{malformed","native_text":text,"transported":false}));
        }
        peer.close();
    }
    let direct = root.join("raw-nonobjects");
    seed(&direct);
    let registry = Registry::builtin();
    for value in [
        Value::Null,
        json!([]),
        json!(7),
        json!("text"),
        json!(false),
    ] {
        for tool in registry.tools() {
            let text = registry.execute(
                &direct.join("workspace"),
                &tool.name,
                &value.to_string(),
                &[],
            );
            let empty_object_text =
                registry.execute(&direct.join("workspace"), &tool.name, "{}", &[]);
            assert_eq!(text, empty_object_text);
            raw.push(json!({"name":tool.name,"raw":value.to_string(),"native_text":text,"same_as_empty_object":true,"transported":false}));
        }
    }
}

fn invalid_wire(root: &Path) -> Vec<Value> {
    seed(root);
    let (mut child, tree) = workload_process::spawn(
        command(root, &["--server", "auto", "normal"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut lease = Lease::new(child, tree, PEER_LIMIT);
    input.write_all(b"{malformed\n").unwrap();
    input.flush().unwrap();
    let parse = read_frame(&mut output).unwrap();
    assert_eq!(parse["error"]["code"], -32700);
    write_frame(
        &mut input,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"invalid-wire-fixture","version":"1"}}}),
    );
    assert!(read_frame(&mut output).unwrap().get("result").is_some());
    write_frame(
        &mut input,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    let mut rows = vec![parse];
    for (i, args) in [
        Value::Null,
        json!([]),
        json!(7),
        json!("text"),
        json!(false),
    ]
    .into_iter()
    .enumerate()
    {
        write_frame(
            &mut input,
            &json!({"jsonrpc":"2.0","id":i+2,"method":"tools/call","params":{"name":"list_dir","arguments":args}}),
        );
        let response = read_frame(&mut output).unwrap();
        assert_eq!(response["error"]["code"], -32602);
        rows.push(response);
    }
    for (i, params) in [
        json!([]),
        json!({"name":7,"arguments":{}}),
        json!({}),
        json!({"name":"unknown_tool","arguments":{}}),
    ]
    .into_iter()
    .enumerate()
    {
        write_frame(
            &mut input,
            &json!({"jsonrpc":"2.0","id":20+i,"method":"tools/call","params":params}),
        );
        let response = read_frame(&mut output).unwrap();
        assert_eq!(response["error"]["code"], -32602);
        rows.push(response);
    }
    write_frame(
        &mut input,
        &json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"write_to_file","arguments":{"path":"notification.txt","content":"must not run"}}}),
    );
    write_frame(
        &mut input,
        &json!({"jsonrpc":"2.0","id":30,"method":"ping"}),
    );
    assert_eq!(
        read_frame(&mut output).unwrap(),
        json!({"jsonrpc":"2.0","id":30,"result":{}})
    );
    assert!(
        !root.join("calls.jsonl").exists(),
        "invalid wire must not reach a native handler"
    );
    assert!(!root.join("workspace/notification.txt").exists());
    // MCP arguments is optional. Omission means the native empty object;
    // explicit null/nonobjects above remain invalid MCP arguments.
    write_frame(
        &mut input,
        &json!({"jsonrpc":"2.0","id":31,"method":"tools/call","params":{"name":"list_dir"}}),
    );
    let omitted = read_frame(&mut output).unwrap();
    assert_eq!(
        omitted["result"]["content"][0]["text"],
        Registry::builtin().execute(&root.join("workspace"), "list_dir", "{}", &[])
    );
    assert_eq!(call_count(root), 1);
    lease.close();
    rows
}

fn lost_mutation(root: &Path) -> Value {
    seed(root);
    let began = Instant::now();
    let mut peer = Peer::open(root, "auto", "lost", Duration::from_secs(2));
    let result = peer.host.call_tool(
        "fixture",
        "append_to_file",
        &json!({"path":"once.txt","content":"one mutation"}),
        None,
    );
    assert!(result.is_err(), "lost reply must not become success");
    assert!(began.elapsed() < Duration::from_secs(5));
    peer.close();
    assert_eq!(
        fs::read_to_string(root.join("workspace/once.txt")).unwrap(),
        "one mutation"
    );
    let calls = fs::read_to_string(root.join("calls.jsonl")).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "neither retry nor native fallback is allowed"
    );
    assert!(root.join("descendant.ready").exists());
    fs::write(root.join("probe"), []).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !root.join("descendant.survived").exists(),
        "tool descendant survived owner closure"
    );
    json!({"call_count":1,"file_bytes":"one mutation","transport_error":result.unwrap_err().to_string(),"replayed":false,"descendant_stopped":true})
}

fn suite(root: &Path) {
    owned(root);
    assert_eq!(std::env::var("OPENAI_API_KEY").unwrap(), SYNTHETIC_KEY);
    assert!(std::env::var_os("SUSI_HOST_TOKEN").is_none());
    let mut comparisons = Vec::new();
    let mut raw = Vec::new();
    compare_objects(&root.join("objects"), &mut comparisons);
    policies_and_raw(&root.join("policies"), &mut comparisons, &mut raw);
    let invalid = invalid_wire(&root.join("invalid-wire"));
    let lost = lost_mutation(&root.join("lost-mutation"));
    let evidence = json!({"status":"passed","custom_harness":true,"valid_object_comparisons":comparisons.len(),"transported_comparisons":comparisons.iter().filter(|row| row["transported"] == true).count(),"local_refusal_comparisons":comparisons.iter().filter(|row| row["transported"] == false).count(),"direct_raw_characterizations":raw.len(),"invalid_wire_rejections":invalid.len(),"comparisons":comparisons,"raw_native_cases":raw,"invalid_wire":invalid,"lost_mutation":lost,"platform":"x86_64-linux","production_changes":0,"reduction_credit":0,"scope":"Valid-object transport and native-handler preservation; no production worker backend or migration.","unresolved_raw_contract":[{"input":"malformed JSON","native":"authorize before raw parse; denial wins","MCP":"parse error before tool dispatch","normalization":"none; future client-side preservation required"},{"input":"nonobject JSON","native":"current six handlers behave like empty object, including list_dir root listing","MCP":"invalid params; object required","normalization":"characterized only, not implemented"}],"not_qualified":["Windows/native platforms other than x86_64 Linux","production worker transport integration","large command output, command timeout and existing path-race behavior","full protocol conformance or release qualification","typed MCP isError mapping: native handlers return untyped strings","outer-suite forced termination is bounded by child self-expiry, not a demonstrated immediate nested-group cleanup"]});
    fs::write(
        root.join("evidence.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    println!(
        "{}",
        json!({"status":"passed","custom_harness":true,"valid_object_comparisons":evidence["valid_object_comparisons"],"transported_comparisons":evidence["transported_comparisons"],"local_refusal_comparisons":evidence["local_refusal_comparisons"],"direct_raw_characterizations":evidence["direct_raw_characterizations"],"invalid_wire_rejections":evidence["invalid_wire_rejections"],"evidence":root.join("evidence.json")})
    );
}

fn run_parent() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = fixture_root().join(format!("{}-{stamp}", std::process::id()));
    prepare(&root);
    let (child, tree) = workload_process::spawn(
        command(&root, &["--suite"])
            .stdout(fs::File::create(root.join("suite.stdout")).unwrap())
            .stderr(fs::File::create(root.join("suite.stderr")).unwrap()),
    )
    .unwrap();
    let mut lease = Lease::new(child, tree, Duration::from_secs(90));
    loop {
        let status = {
            let mut owner = lease.owner.lock().unwrap();
            if owner.status.is_none() {
                let OwnedChild {
                    child,
                    tree,
                    status,
                } = &mut *owner;
                *status = workload_process::try_wait(tree, child).unwrap();
            }
            owner.status
        };
        if let Some(status) = status {
            lease.close();
            print!("{}", fs::read_to_string(root.join("suite.stdout")).unwrap());
            if !status.success() {
                eprintln!("{}", fs::read_to_string(root.join("suite.stderr")).unwrap());
                eprintln!(
                    "{}",
                    json!({"status":"failed","custom_harness":true,"evidence_directory":root})
                );
                std::process::exit(1);
            }
            assert!(root.join("evidence.json").is_file());
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, policy, behavior, root] if mode == "--server" => {
            server(Path::new(root), policy, behavior)
        }
        [mode, root] if mode == "--suite" => suite(Path::new(root)),
        [mode, root] if mode == "--descendant" => {
            let root = Path::new(root);
            owned(root);
            fs::write(root.join("descendant.ready"), []).unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                if root.join("probe").exists() {
                    fs::write(root.join("descendant.survived"), []).unwrap();
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        [mode] if mode == "--run" && cfg!(all(target_os = "linux", target_arch = "x86_64")) => {
            run_parent()
        }
        _ if args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "--run" | "--suite" | "--server" | "--descendant"
            )
        }) =>
        {
            eprintln!(
                "{}",
                json!({"status":"failed","custom_harness":true,"reason":"Invalid fixture invocation or unsupported native platform"})
            );
            std::process::exit(2);
        }
        _ => println!(
            "{}",
            json!({"status":"skipped","custom_harness":true,"reason":"Opt-in x86_64 Linux proof: cargo test --test harness_worker_mcp_parity -- --run"})
        ),
    }
}
