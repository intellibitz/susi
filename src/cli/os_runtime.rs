//! `susi os runtime` — what is actually running and resident on this host,
//! and which of it SUSI owns. Discovery (`susi os ecosystem`) answers "what
//! is installed"; this view answers "what is live, who owns it, and can
//! SUSI control it":
//!
//! - **processes** — AI-runtime processes classified by ownership. A process
//!   is `susi`-owned only when its parent chain reaches the running daemon;
//!   everything else is `external` and reported as observable, not
//!   controllable.
//! - **models** — managed model files (installed), which processes have them
//!   mapped or open (observed residency), and which one is selected.
//! - **gpu** — per-process GPU memory from `nvidia-smi`, when available.
//!
//! Residency is observed from `/proc/<pid>/maps` and `/proc/<pid>/fd`: a model
//! read fully into memory and then closed leaves no trace there, so an empty
//! `open_by` means "not mapped or open", never "not loaded".

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Runtimes identified by the basename of the executable (argv[0]).
const RUNTIME_BINARIES: &[&str] = &[
    "ollama",
    "llama-server",
    "llama-cli",
    "vllm",
    "sglang",
    "tritonserver",
    "lmdeploy",
    "text-generation-launcher",
    "koboldcpp",
    "localai",
    "lm-studio",
];

/// Interpreter-hosted runtimes identified by a marker in their arguments.
const INTERPRETERS: &[&str] = &[
    "python", "python3", "node", "uv", "uvx", "npx", "bun", "deno",
];
const HOSTED_MARKERS: &[(&str, &str)] = &[
    ("vllm", "vllm"),
    ("sglang", "sglang"),
    ("lmdeploy", "lmdeploy"),
    ("text_generation_server", "tgi"),
    ("llama_cpp", "llama.cpp"),
    ("modelcontextprotocol", "mcp-server"),
    ("mcp-server", "mcp-server"),
    ("mcp_server", "mcp-server"),
];

/// Model artifacts worth reporting when a process maps or opens them.
fn is_model_artifact(path: &str) -> bool {
    path.ends_with(".gguf")
        || path.ends_with(".safetensors")
        || path.ends_with(".onnx")
        || path.contains("/.ollama/models/blobs/")
}

/// Parent pid from `/proc/<pid>/stat`. The comm field may contain spaces and
/// parentheses, so fields are counted from the last `)`.
fn parse_ppid(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// Classify a process by its argv and resolved executable. `None` means the
/// process is not part of the AI ecosystem and is left out of the view.
fn classify(argv: &[String], exe: Option<&str>) -> Option<&'static str> {
    let argv0 = basename(argv.first()?.as_str());
    let exe_name = exe.map(|e| basename(e.trim_end_matches(" (deleted)")));
    let is_susi = argv0 == "susi" || exe_name == Some("susi");
    if is_susi {
        return Some(if argv.iter().any(|a| a == "daemon-start") {
            "susi-daemon"
        } else if argv.iter().any(|a| a == "service-run") {
            "susi-service"
        } else {
            "susi"
        });
    }
    if let Some(runtime) = RUNTIME_BINARIES.iter().find(|name| **name == argv0) {
        return Some(runtime);
    }
    let interpreter = INTERPRETERS
        .iter()
        .any(|name| argv0 == *name || argv0.starts_with("python3."));
    if !interpreter {
        return None;
    }
    argv.iter().skip(1).find_map(|arg| {
        HOSTED_MARKERS
            .iter()
            .find(|(marker, _)| arg.contains(marker))
            .map(|(_, kind)| *kind)
    })
}

/// True when `pid` is the daemon or one of its descendants. Bounded so a
/// pid-reuse cycle in a racing `/proc` snapshot cannot loop forever.
fn owned_by(pid: u32, daemon: Option<u32>, parents: &HashMap<u32, u32>) -> bool {
    let Some(daemon) = daemon else {
        return false;
    };
    let mut current = pid;
    for _ in 0..64 {
        if current == daemon {
            return true;
        }
        match parents.get(&current) {
            Some(&parent) if parent != 0 && parent != current => current = parent,
            Some(_) | None => return false,
        }
    }
    false
}

struct Proc {
    pid: u32,
    argv: Vec<String>,
    exe: Option<String>,
}

fn read_proc(pid: u32) -> Option<(Proc, u32)> {
    let dir = PathBuf::from(format!("/proc/{pid}"));
    let ppid = parse_ppid(&std::fs::read_to_string(dir.join("stat")).ok()?)?;
    let raw = std::fs::read(dir.join("cmdline")).ok()?;
    let argv = raw
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    let exe = std::fs::read_link(dir.join("exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    Some((Proc { pid, argv, exe }, ppid))
}

/// Model artifacts each process has mapped or open. Unreadable processes
/// (other users) are skipped rather than guessed at.
fn artifacts_in_use(pid: u32) -> BTreeSet<String> {
    let dir = PathBuf::from(format!("/proc/{pid}"));
    let mut found = BTreeSet::new();
    if let Ok(maps) = std::fs::read_to_string(dir.join("maps")) {
        for line in maps.lines() {
            if let Some(path) = line.split_whitespace().nth(5) {
                if is_model_artifact(path) {
                    found.insert(path.to_string());
                }
            }
        }
    }
    if let Ok(fds) = std::fs::read_dir(dir.join("fd")) {
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let target = target.to_string_lossy().into_owned();
                if is_model_artifact(&target) {
                    found.insert(target);
                }
            }
        }
    }
    found
}

/// Per-pid GPU memory (MiB) from `nvidia-smi`; empty when unavailable.
fn gpu_memory_by_pid() -> BTreeMap<u32, u64> {
    let Ok(output) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
    else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    parse_gpu_apps(&String::from_utf8_lossy(&output.stdout))
}

fn parse_gpu_apps(csv: &str) -> BTreeMap<u32, u64> {
    csv.lines()
        .filter_map(|line| {
            let (pid, mem) = line.split_once(',')?;
            Some((pid.trim().parse().ok()?, mem.trim().parse().ok()?))
        })
        .fold(BTreeMap::new(), |mut acc, (pid, mem): (u32, u64)| {
            *acc.entry(pid).or_insert(0) += mem;
            acc
        })
}

/// Build the runtime view as JSON. Shared by `susi os runtime` and the
/// `running_ai_processes` section of `susi os ecosystem`.
pub(crate) fn view() -> serde_json::Value {
    let daemon = susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir())
        .map(|d| d.pid);
    let services: HashMap<u32, String> = susi_core::service_table::status()
        .into_iter()
        .filter_map(|s| Some((s.pid?, s.name)))
        .collect();
    let gpu = gpu_memory_by_pid();
    let self_pid = std::process::id();

    let mut parents = HashMap::new();
    let mut procs = Vec::new();
    let mut in_use: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
                continue;
            };
            let Some((proc_, ppid)) = read_proc(pid) else {
                continue;
            };
            parents.insert(pid, ppid);
            for artifact in artifacts_in_use(pid) {
                in_use.entry(artifact).or_default().insert(pid);
            }
            procs.push(proc_);
        }
    }

    let processes: Vec<_> = procs
        .iter()
        .filter(|p| p.pid != self_pid)
        .filter_map(|p| {
            let kind = classify(&p.argv, p.exe.as_deref())?;
            let owned = owned_by(p.pid, daemon, &parents);
            let service = services.get(&p.pid);
            let control = match (owned, service) {
                (true, Some(name)) => Some(format!("susi os manage service <stop|restart> {name}")),
                (true, None) if kind == "susi-daemon" => Some("susi restart".to_string()),
                (true, None) if kind == "mcp-server" => {
                    Some("susi os manage mcp <status|uninstall> <name>".to_string())
                }
                (true, None) | (false, _) => None,
            };
            Some(serde_json::json!({
                "pid": p.pid,
                "kind": kind,
                "owner": if owned { "susi" } else { "external" },
                "service": service,
                "controllable": control.is_some(),
                "control": control,
                "gpu_mib": gpu.get(&p.pid),
                "stale_binary": p.exe.as_deref().is_some_and(|e| e.ends_with(" (deleted)")),
                "command": p.argv.join(" ").chars().take(300).collect::<String>(),
            }))
        })
        .collect();

    let models_dir = susi_gemi::ModelManager::get_models_dir();
    let managed_root = models_dir
        .canonicalize()
        .unwrap_or_else(|_| models_dir.clone());
    let selected = susi_gemi::ModelManager::get_selected_model(None);
    let mut managed = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&models_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !path.is_file() || !is_model_artifact(&name) {
                continue;
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            let key = canonical.to_string_lossy().into_owned();
            managed.push(serde_json::json!({
                "name": name,
                "path": key,
                "bytes": entry.metadata().map(|m| m.len()).ok(),
                "installed": true,
                "selected": selected.as_deref().is_some_and(|s| s == name || s == key),
                "open_by": in_use.get(&key).map(|pids| pids.iter().collect::<Vec<_>>()).unwrap_or_default(),
            }));
        }
    }
    managed.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let external_models: Vec<_> = in_use
        .iter()
        .filter(|(path, _)| !Path::new(path).starts_with(&managed_root))
        .map(|(path, pids)| serde_json::json!({ "path": path, "open_by": pids }))
        .collect();

    let owned = processes.iter().filter(|p| p["owner"] == "susi").count();
    let stale = processes
        .iter()
        .filter(|p| p["stale_binary"] == true)
        .count();
    serde_json::json!({
        "daemon_pid": daemon,
        "processes": processes,
        "models": {
            "managed_dir": models_dir,
            "managed": managed,
            "external_in_use": external_models,
        },
        "gpu_mib_by_pid": gpu,
        "summary": {
            "processes": processes.len(),
            "susi_owned": owned,
            "external": processes.len() - owned,
            "stale_binaries": stale,
            "managed_models": managed.len(),
            "managed_models_in_use": managed.iter().filter(|m| m["open_by"].as_array().is_some_and(|a| !a.is_empty())).count(),
        },
        "note": "residency is observed from /proc maps and open fds; a model read into memory and closed is not visible there",
    })
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
            "  {}{}{}",
            m["name"].as_str().unwrap_or_default(),
            if m["selected"] == true {
                "  [selected]"
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
    for m in models["external_in_use"].as_array().into_iter().flatten() {
        println!(
            "  external: {}  [open by {}]",
            m["path"].as_str().unwrap_or_default(),
            m["open_by"]
        );
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

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn ppid_survives_parens_and_spaces_in_comm() {
        assert_eq!(parse_ppid("42 (a) b) c) S 7 42 42 0"), Some(7));
        assert_eq!(parse_ppid("1 (systemd) S 0 1 1"), Some(0));
        assert_eq!(parse_ppid("garbage"), None);
    }

    #[test]
    fn classifies_susi_runtimes_and_hosted_servers() {
        let exe = Some("/home/u/.susi/bin/susi (deleted)");
        assert_eq!(
            classify(
                &argv(&["/proc/self/exe", "service-run", "susi-native"]),
                exe
            ),
            Some("susi-service")
        );
        assert_eq!(
            classify(&argv(&["susi", "daemon-start"]), None),
            Some("susi-daemon")
        );
        assert_eq!(
            classify(&argv(&["/usr/bin/ollama", "serve"]), None),
            Some("ollama")
        );
        assert_eq!(
            classify(
                &argv(&["python3.12", "-m", "vllm.entrypoints.api_server"]),
                None
            ),
            Some("vllm")
        );
        assert_eq!(
            classify(
                &argv(&["node", "/x/@modelcontextprotocol/server-filesystem"]),
                None
            ),
            Some("mcp-server")
        );
        assert_eq!(classify(&argv(&["python3", "script.py"]), None), None);
        assert_eq!(classify(&argv(&["/usr/bin/kate", "susi.rs"]), None), None);
    }

    #[test]
    fn ownership_follows_the_parent_chain_to_the_daemon() {
        let parents = HashMap::from([(30, 20), (20, 10), (10, 1), (1, 0), (40, 1)]);
        assert!(owned_by(30, Some(10), &parents));
        assert!(owned_by(10, Some(10), &parents));
        assert!(!owned_by(40, Some(10), &parents));
        assert!(!owned_by(30, None, &parents));
        let cycle = HashMap::from([(5, 6), (6, 5)]);
        assert!(!owned_by(5, Some(99), &cycle));
    }

    #[test]
    fn gpu_apps_sum_per_pid_and_skip_malformed_rows() {
        let parsed = parse_gpu_apps("123, 512\n123, 256\nbad row\n7, 1024\n");
        assert_eq!(parsed.get(&123), Some(&768));
        assert_eq!(parsed.get(&7), Some(&1024));
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn model_artifacts_include_ollama_blobs() {
        assert!(is_model_artifact("/m/qwen.gguf"));
        assert!(is_model_artifact("/h/.ollama/models/blobs/sha256-abc"));
        assert!(!is_model_artifact("/usr/lib/libc.so.6"));
    }
}
