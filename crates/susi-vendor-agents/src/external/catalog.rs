use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogKind {
    /// Top coding executors (Claude Code, Cursor, …).
    Execution,
    /// Top agent frameworks / engines (LangGraph, CrewAI, …).
    Framework,
}

impl CatalogKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Execution => "agent",
            Self::Framework => "framework",
        }
    }

    pub fn list_hint(self) -> &'static str {
        match self {
            Self::Execution => "`susi agents list`",
            Self::Framework => "`susi frameworks list`",
        }
    }

    pub fn config_subdir(self) -> &'static str {
        match self {
            Self::Execution => "execution-agents",
            Self::Framework => "agent-engines",
        }
    }

    pub fn run_subdir(self) -> &'static str {
        match self {
            Self::Execution => "execution-agents",
            Self::Framework => "agent-engines",
        }
    }

    fn bundled(self) -> &'static str {
        match self {
            Self::Execution => include_str!("../../../../config/execution-agents.json"),
            Self::Framework => include_str!("../../../../config/agent-engines.json"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Adapter {
    Command {
        program: String,
        args: Vec<String>,
    },
    Qwen {
        python: String,
    },
    /// User-authored framework entry (script or module:callable) via absolute config JSON.
    Python {
        python: String,
        /// Import name checked at doctor/preflight time (e.g. `langgraph`, `pydantic_ai`).
        import_name: String,
        config_env: String,
    },
    Devin {
        api_key_env: String,
        /// Env var holding the Devin organization id (`org-…`). Defaults to `DEVIN_ORG_ID`.
        #[serde(default = "default_devin_org_id_env")]
        org_id_env: String,
    },
    Manus {
        api_key_env: String,
    },
    /// Remote A2A-compliant agent, discovered from `<url>/.well-known/agent-card.json`.
    A2a {
        url: String,
        /// Env var holding a bearer token, when the remote agent requires one.
        #[serde(default)]
        token_env: Option<String>,
    },
    /// OpenHands app-server or Cloud REST API (`/api/v1/app-conversations`).
    OpenHandsServer {
        base_url: String,
        /// Env var holding the OpenHands API key (bearer), if the server needs one.
        #[serde(default)]
        api_key_env: Option<String>,
    },
    /// Agent Client Protocol agent spoken to over stdio (JSON-RPC).
    Acp {
        program: String,
        args: Vec<String>,
    },
}

fn default_devin_org_id_env() -> String {
    "DEVIN_ORG_ID".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    pub peer_name: String,
    /// User-supplied catalog order, not a measured quality score.
    pub rank: u8,
    pub documentation: String,
    pub adapter: Adapter,
    /// Mandate 35: preserve unknown catalog keys from extension packs.
    #[serde(flatten, default)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

pub fn catalog(kind: CatalogKind) -> Result<Vec<AgentDefinition>> {
    let name = match kind {
        CatalogKind::Execution => "execution-agents.json",
        CatalogKind::Framework => "agent-engines.json",
    };
    Ok(crate::susi_sandbox::extensions::load_json_or_bundled(
        name,
        kind.bundled(),
    ))
}

pub fn definition(kind: CatalogKind, id: &str) -> Result<AgentDefinition> {
    catalog(kind)?
        .into_iter()
        .find(|a| a.id == id || a.peer_name == id)
        .with_context(|| format!("unknown {} {id}; use {}", kind.label(), kind.list_hint()))
}

/// Resolve a managed peer name against executors first, then frameworks.
pub fn resolve_managed(id: &str) -> Result<(CatalogKind, AgentDefinition)> {
    if let Ok(def) = definition(CatalogKind::Execution, id) {
        return Ok((CatalogKind::Execution, def));
    }
    definition(CatalogKind::Framework, id).map(|def| (CatalogKind::Framework, def))
}

impl Adapter {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Command { program, args } => {
                if program.trim().is_empty()
                    || program.contains('\0')
                    || args.iter().any(|a| a.contains('\0'))
                {
                    bail!("command and arguments must be nonempty executable / NUL-free argv");
                }
            }
            Self::Qwen { python } if python.trim().is_empty() => {
                bail!("python executable is required")
            }
            Self::Python {
                python,
                import_name,
                config_env,
            } => {
                if python.trim().is_empty() {
                    bail!("python executable is required");
                }
                if import_name.is_empty()
                    || !import_name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
                {
                    bail!("import_name must be a dotted Python module name");
                }
                if config_env.is_empty()
                    || !config_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!(
                        "config_env must name an environment variable, not contain a path or secret"
                    );
                }
            }
            Self::Devin {
                api_key_env,
                org_id_env,
            } => {
                if api_key_env.is_empty()
                    || !api_key_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!(
                        "api_key_env must name an environment variable, not contain a credential"
                    );
                }
                if org_id_env.is_empty()
                    || !org_id_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!("org_id_env must name an environment variable, not contain an org id");
                }
            }
            Self::Manus { api_key_env } => {
                if api_key_env.is_empty()
                    || !api_key_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!(
                        "api_key_env must name an environment variable, not contain a credential"
                    );
                }
            }
            Self::A2a { url, token_env } => {
                if !(url.starts_with("https://") || url.starts_with("http://"))
                    || url.contains('\0')
                {
                    bail!("A2A url must be an http(s) URL");
                }
                if token_env.as_deref().is_some_and(|e| {
                    e.is_empty() || !e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                }) {
                    bail!("token_env must name an environment variable, not contain a credential");
                }
            }
            Self::OpenHandsServer {
                base_url,
                api_key_env,
            } => {
                if !(base_url.starts_with("https://") || base_url.starts_with("http://"))
                    || base_url.contains('\0')
                {
                    bail!("OpenHands base_url must be an http(s) URL");
                }
                if api_key_env.as_deref().is_some_and(|e| {
                    e.is_empty() || !e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                }) {
                    bail!(
                        "api_key_env must name an environment variable, not contain a credential"
                    );
                }
            }
            Self::Acp { program, args } => {
                if program.trim().is_empty()
                    || program.contains('\0')
                    || args.iter().any(|a| a.contains('\0'))
                {
                    bail!("ACP command and arguments must be nonempty executable / NUL-free argv");
                }
            }
            Self::Qwen { .. } => {}
        }
        Ok(())
    }

    /// Local preflight only. Presence of a binary/package does not prove vendor authentication.
    pub fn preflight(&self) -> Result<String> {
        self.validate()?;
        match self {
            Self::Command { program, .. } => {
                resolve_program(program)
                    .with_context(|| format!("missing executable: {program}"))?;
                Ok("executable found; vendor login/permissions must be configured".into())
            }
            Self::A2a { url, token_env } => {
                if let Some(env) = token_env {
                    if crate::susi_config::env_or_cloud_env(env)
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
                    {
                        bail!("set {env} for the remote A2A agent's bearer token");
                    }
                }
                Ok(format!(
                    "A2A endpoint {url}; agent card is fetched at execution"
                ))
            }
            Self::OpenHandsServer {
                base_url,
                api_key_env,
            } => {
                if let Some(env) = api_key_env {
                    if crate::susi_config::env_or_cloud_env(env)
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
                    {
                        bail!("set {env} for the OpenHands API");
                    }
                }
                Ok(format!(
                    "OpenHands endpoint {base_url}; API access checked at execution"
                ))
            }
            Self::Acp { program, .. } => {
                resolve_program(program)
                    .with_context(|| format!("missing ACP executable: {program}"))?;
                Ok(
                    "ACP executable found; agent auth is checked by the initialize handshake"
                        .into(),
                )
            }
            Self::Qwen { python } => {
                resolve_program(python).context("Python executable missing")?;
                if std::env::var("SUSI_QWEN_CONFIG")
                    .unwrap_or_default()
                    .is_empty()
                {
                    bail!("set SUSI_QWEN_CONFIG to an absolute JSON configuration file for Qwen-Agent");
                }
                Ok("Python/config present; qwen_agent package and model access checked at execution".into())
            }
            Self::Python {
                python,
                import_name,
                config_env,
            } => {
                let program = resolve_program(python).context("Python executable missing")?;
                let importable = match scan_import_verdict(&program, import_name) {
                    Some(ImportVerdict::Importable) => true,
                    Some(ImportVerdict::NotImportable) => false,
                    Some(ImportVerdict::TimedOut) => bail!(
                        "failed to probe import {import_name}: it did not finish within {} s",
                        PER_IMPORT.as_secs()
                    ),
                    None => crate::susi_core::bounded_cmd::output_within(
                        Command::new(&program).args(["-c", &format!("import {import_name}")]),
                        PER_IMPORT,
                    )
                    .with_context(|| format!("failed to probe import {import_name}"))?
                    .status
                    .success(),
                };
                if !importable {
                    bail!("Python package not importable: {import_name}");
                }
                let path = std::env::var(config_env).unwrap_or_default();
                if path.trim().is_empty() {
                    bail!("set {config_env} to an absolute JSON config with script or entrypoint");
                }
                let config = Path::new(&path);
                if !config.is_absolute() {
                    bail!("{config_env} must be an absolute path");
                }
                if !config.is_file() {
                    bail!("{config_env} does not point to an existing file");
                }
                Ok("Python package and entry config present; model credentials checked at execution".into())
            }
            Self::Devin {
                api_key_env,
                org_id_env,
            } => {
                if crate::susi_config::env_or_cloud_env(api_key_env)
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    bail!("set {api_key_env} for cloud API access");
                }
                if crate::susi_config::env_or_cloud_env(org_id_env)
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    bail!("set {org_id_env} (Devin org id, e.g. org-…) for API v3");
                }
                Ok("credential and org id present; API access checked at execution".into())
            }
            Self::Manus { api_key_env } => {
                if crate::susi_config::env_or_cloud_env(api_key_env)
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    bail!("set {api_key_env} for cloud API access");
                }
                Ok("credential present; API access checked at execution".into())
            }
        }
    }

    pub fn is_cloud(&self) -> bool {
        matches!(
            self,
            Self::Devin { .. }
                | Self::Manus { .. }
                | Self::A2a { .. }
                | Self::OpenHandsServer { .. }
        )
    }
}

/// How long one Python import may take before its probe gives up, in the batch
/// as in the single probe it replaces.
const PER_IMPORT: std::time::Duration = std::time::Duration::from_secs(10);

/// One module's answer from a batch probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImportVerdict {
    Importable,
    NotImportable,
    /// The import did not finish within [`PER_IMPORT`]; the single probe would
    /// have timed out the same way.
    TimedOut,
}

type ImportVerdicts = std::collections::HashMap<(PathBuf, String), ImportVerdict>;

thread_local! {
    /// Import verdicts for the readiness scan running on this thread, keyed by
    /// interpreter and module. Set only for the length of one scan
    /// ([`with_python_imports`]): a daemon that rescans must still see a
    /// framework installed after its previous scan.
    static SCAN_IMPORTS: std::cell::RefCell<Option<ImportVerdicts>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn scan_import_verdict(program: &Path, module: &str) -> Option<ImportVerdict> {
    SCAN_IMPORTS.with(|scan| {
        scan.borrow()
            .as_ref()?
            .get(&(program.to_path_buf(), module.to_string()))
            .copied()
    })
}

/// Run `scan` with every Python import the `adapters` declare probed up front,
/// in one interpreter per program. Each Python preflight used to start its own
/// `python -c "import X"`, about 50 ms apiece: sixteen of them on every susi
/// command, which primes the catalogs, so about half of each launch. A module
/// the batch gives no answer for is left to the single probe.
pub fn with_python_imports<T>(adapters: &[Adapter], scan: impl FnOnce() -> T) -> T {
    let mut wanted: std::collections::BTreeMap<PathBuf, Vec<String>> = Default::default();
    for adapter in adapters {
        if let Adapter::Python {
            python,
            import_name,
            ..
        } = adapter
        {
            if let Some(program) = resolve_program(python) {
                wanted.entry(program).or_default().push(import_name.clone());
            }
        }
    }
    let mut verdicts = ImportVerdicts::new();
    for (program, modules) in wanted {
        for (module, verdict) in probe_imports(&program, &modules, PER_IMPORT) {
            verdicts.insert((program.clone(), module), verdict);
        }
    }
    with_verdicts(verdicts, scan)
}

/// Run `scan` with `verdicts` stored for this thread, and clear them when it
/// returns or unwinds.
fn with_verdicts<T>(verdicts: ImportVerdicts, scan: impl FnOnce() -> T) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            SCAN_IMPORTS.with(|scan| scan.borrow_mut().take());
        }
    }
    SCAN_IMPORTS.with(|scan| *scan.borrow_mut() = Some(verdicts));
    let _clear = Clear;
    scan()
}

/// Run `scan` with one stored verdict, as a scan would have stored it.
#[cfg(test)]
pub(super) fn with_scan_verdicts_for_test<T>(
    program: &Path,
    module: &str,
    verdict: ImportVerdict,
    scan: impl FnOnce() -> T,
) -> T {
    let mut verdicts = ImportVerdicts::new();
    verdicts.insert((program.to_path_buf(), module.to_string()), verdict);
    with_verdicts(verdicts, scan)
}

/// The batch probe. One interpreter starts, then forks a fresh child per
/// module. The child takes the batch's own names back out of `__main__` and
/// runs there exactly what the single probe ran: `import X` (as source, so the
/// import statement's own rules apply), with `sys.argv` as `-c` sets it and no
/// descriptor the single probe would not have. An import that succeeds ends the
/// script, so it exits the way `python -c "import X"` would (atexit, threads
/// left running, PYTHONINSPECT); a SystemExit keeps its code. Any other error
/// exits 1 at once, skipping the interpreter shutdown that cost about 30 ms for
/// each framework not installed: an import that fails is not importable here
/// even where `python -c` would then hang in its shutdown (a non-daemon thread)
/// or exit 0 from it (an atexit hook). What is left different is accepted: the
/// child's output goes to the null device rather than a pipe, the batch's own
/// frames sit above the importing frame, and `sys.orig_argv` is the batch's.
///
/// The parent imports nothing a file in the working directory could stand in
/// for (`os` and `sys` are loaded at startup, `time` is built in). No import's
/// output can pass for an answer: the import writes only to the null device,
/// and the parent reports each module on a line of its own, tagged with a
/// per-run token. It kills a child that runs past the per-import bound. It
/// announces each module before forking it, so no import runs before the
/// caller knows it is in flight; a fork that fails is reported as `nofork`,
/// and that module and the rest get the single probe.
///
/// The batch sits in a process group of its own (`own-group`), which a
/// terminal's Ctrl-C does not reach, so it watches its caller instead: while
/// an import runs it writes an empty line to its stdout about every 50 ms (the
/// caller skips untagged lines), and a broken pipe means the caller is gone,
/// whatever runs between them (a wrapper script that does not exec). stdin
/// stays the null device the single probe has. The batch then kills the
/// import, even one that left the group, and the whole group with it, so
/// anything the import started goes too, as the Ctrl-C would have killed the
/// single probe's.
pub(super) const BATCH_SCRIPT: &str = "keep = set(globals())
import os, sys, time
token, bound, own_group = sys.argv[1], float(sys.argv[2]), sys.argv[3] == 'own-group'
def say(kind, m):
    sys.stdout.write('\\n%s %s %s\\n' % (token, kind, m))
    sys.stdout.flush()
def caller_gone():
    try:
        os.write(1, b'\\n')
    except OSError:
        return True
    return False
def batch():
    for m in sys.argv[4:]:
        say('begin', m)
        try:
            pid = os.fork()
        except OSError:
            say('nofork', m)
            return None
        if pid == 0:
            null = os.open(os.devnull, os.O_RDWR)
            for fd in (0, 1, 2):
                os.dup2(null, fd)
            if null > 2:
                os.close(null)
            sys.argv = ['-c']
            return m
        deadline = time.monotonic() + bound
        beat = 0.0
        while True:
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                ok = os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0
                say('ok' if ok else 'fail', m)
                break
            now = time.monotonic()
            if now > beat:
                beat = now + 0.05
                if caller_gone():
                    os.kill(pid, 9)
                    if own_group:
                        os.killpg(0, 9)
                    os._exit(1)
            if now > deadline:
                os.kill(pid, 9)
                os.waitpid(pid, 0)
                say('timeout', m)
                break
            time.sleep(0.002)
    say('done', '-')
def run(m, main=globals(), keep=keep, _exit=os._exit):
    for name in [n for n in main if n not in keep]:
        del main[name]
    try:
        exec('import ' + m, main)
    except SystemExit:
        raise
    except BaseException:
        _exit(1)
child = batch()
if child is not None:
    run(child)
";

/// How one batch interpreter ended.
enum BatchEnd {
    /// Every module answered.
    Done,
    /// The interpreter stopped answering while this module was being probed;
    /// it was killed, and the modules after it still need a batch.
    Stuck(String),
    /// The interpreter ended without finishing. An import that killed it names
    /// its module, which is left to the single probe while the rest get a new
    /// batch; `None` (a fork refused, no interpreter at all) leaves everything
    /// undecided to the single probe.
    Died(Option<String>),
}

/// The verdict of every module the batch could decide, with the per-import
/// bound the single probe had: one import that hangs costs one bound, as it
/// did, and the answers before it are kept. A module missing from the result
/// is left to the single probe. Without `os.fork` (Windows) nothing is
/// started: every module takes the single probe.
pub(super) fn probe_imports(
    program: &Path,
    modules: &[String],
    per_import: std::time::Duration,
) -> std::collections::HashMap<String, ImportVerdict> {
    let mut verdicts = std::collections::HashMap::new();
    if cfg!(not(unix)) {
        return verdicts;
    }
    let mut undecided: std::collections::HashSet<String> = Default::default();
    loop {
        let pending: Vec<String> = modules
            .iter()
            .filter(|m| !verdicts.contains_key(*m) && !undecided.contains(*m))
            .cloned()
            .collect();
        if pending.is_empty() {
            break;
        }
        let decided = verdicts.len();
        match run_import_batch(program, &pending, per_import, &mut verdicts) {
            BatchEnd::Done => break,
            BatchEnd::Stuck(module) => {
                verdicts.insert(module, ImportVerdict::TimedOut);
            }
            // A batch that died before deciding anything would die the same
            // way again (fork refused, the interpreter killed at once): one
            // restart per module would double the interpreters the single
            // probe starts. Stop and leave the rest to it.
            BatchEnd::Died(_) if verdicts.len() == decided => break,
            BatchEnd::Died(Some(module)) => {
                undecided.insert(module);
            }
            BatchEnd::Died(None) => break,
        }
    }
    verdicts
}

fn run_import_batch(
    program: &Path,
    modules: &[String],
    per_import: std::time::Duration,
    verdicts: &mut std::collections::HashMap<String, ImportVerdict>,
) -> BatchEnd {
    use std::io::BufRead;
    use std::process::Stdio;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Instant;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let token = format!("susi-import-{}-{nanos}", std::process::id());
    let mut cmd = Command::new(program);
    cmd.arg("-c")
        .arg(BATCH_SCRIPT)
        .arg(&token)
        .arg(per_import.as_secs_f64().to_string())
        .arg("own-group")
        .args(modules)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Its own process group, so an interpreter that stopped answering is
    // killed with the import it was waiting on. A Ctrl-C no longer reaches
    // it; the stdout pipe read below is its lifeline instead, broken once
    // this process is gone.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let Ok(mut child) = cmd.spawn() else {
        return BatchEnd::Died(None);
    };
    let Some(stdout) = child.stdout.take() else {
        kill_batch(&mut child);
        return BatchEnd::Died(None);
    };
    let (tx, rx) = std::sync::mpsc::channel();
    // Detached: an import may hand stdout to a process of its own that
    // outlives the interpreter, and nothing here waits for that.
    // Read bytes, not text: an import may print anything, and its output must
    // not end the batch.
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).split(b'\n') {
            let line = line.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let prefix = format!("{token} ");
    // The interpreter enforces the per-import bound itself and reports a
    // `timeout`; this one, a little longer, only catches an interpreter that
    // stopped answering altogether.
    let bound = per_import + std::time::Duration::from_secs(2);
    let mut current: Option<String> = None;
    let mut deadline = Instant::now() + bound;
    let end = loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Ok(line)) => {
                let Some(rest) = line.trim_end_matches('\r').strip_prefix(&prefix) else {
                    continue;
                };
                let (kind, module) = rest.split_once(' ').unwrap_or((rest, ""));
                match kind {
                    "begin" => {
                        current = Some(module.to_string());
                        deadline = Instant::now() + bound;
                    }
                    "ok" | "fail" | "timeout" if current.as_deref() == Some(module) => {
                        let verdict = match kind {
                            "ok" => ImportVerdict::Importable,
                            "fail" => ImportVerdict::NotImportable,
                            _ => ImportVerdict::TimedOut,
                        };
                        verdicts.insert(module.to_string(), verdict);
                        current = None;
                        deadline = Instant::now() + bound;
                    }
                    // A refused fork would be refused again: no restart.
                    "nofork" if current.as_deref() == Some(module) => break BatchEnd::Died(None),
                    "done" => break BatchEnd::Done,
                    _ => {}
                }
            }
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => break BatchEnd::Died(current),
            Err(RecvTimeoutError::Timeout) => match current {
                Some(module) => break BatchEnd::Stuck(module),
                None => break BatchEnd::Died(None),
            },
        }
    };
    match end {
        // Every answer is in. The interpreter is not waited for (a thread an
        // import left behind); what an import started is left as the single
        // probe left it.
        BatchEnd::Done => {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The import in flight has no parent watching its bound any more.
        // The group kill also ends any process an earlier import of this batch
        // left in the group, which the single probe would have left running;
        // no framework the catalog ships starts one when imported.
        BatchEnd::Stuck(_) | BatchEnd::Died(_) => kill_batch(&mut child),
    }
    end
}

/// Kill a batch interpreter's whole process group (the interpreter, the import
/// it forked, and anything an import left in the group), then reap the
/// interpreter.
#[allow(unsafe_code)]
fn kill_batch(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: kill takes a process-group ID and a constant signal; no pointers.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub fn resolve_program(program: &str) -> Option<PathBuf> {
    fn executable(p: &Path) -> bool {
        if !p.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        true
    }
    let p = Path::new(program);
    if p.components().count() > 1 {
        return executable(p).then(|| p.to_path_buf());
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        // Never silently discover a repo-supplied binary via a relative PATH entry.
        if !dir.is_absolute() {
            continue;
        }
        let p = dir.join(program);
        if executable(&p) {
            return Some(p);
        }
        #[cfg(windows)]
        for ext in ["exe", "cmd", "bat"] {
            let p = dir.join(format!("{program}.{ext}"));
            if executable(&p) {
                return Some(p);
            }
        }
    }
    None
}
