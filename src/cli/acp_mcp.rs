//! Connection-owned MCP servers for ACP sessions. Configuration stays in memory.
use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, TryLockError, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use susi_core::scoped_tools::{self, ScopedTool, ScopedToolProvider, ToolScope};
use susi_gawd::acp::{
    AcpAdmissionPolicy, AcpContentBlock, AcpSessionSpec, AcpTurnContext, AcpTurnExecutor,
};
use susi_gawd_a2a::executor::GawdAcpExecutor;
use susi_wire_client::host::{DuplexStreams, HostPolicy, McpHost};

const POLL: Duration = Duration::from_millis(10);
const MAX_SERVERS: usize = 16;
const MAX_TOOLS: usize = 1024;
const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;

pub(super) struct SessionFactory {
    base: Arc<GawdAcpExecutor>,
    processes: Mutex<Vec<Weak<Process>>>,
}

impl SessionFactory {
    pub(super) fn new(base: GawdAcpExecutor) -> Self {
        Self {
            base: Arc::new(base),
            processes: Mutex::new(Vec::new()),
        }
    }
}

impl AcpTurnExecutor for SessionFactory {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        self.base.execute_turn(ctx, cancelled, emit)
    }

    fn create_session_executor(
        &self,
        spec: &AcpSessionSpec,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<Arc<dyn AcpTurnExecutor>>, String> {
        if spec.mcp_servers.is_empty() {
            return Ok(None);
        }
        if spec.mcp_servers.len() > MAX_SERVERS {
            return Err(format!("ACP session exceeds {MAX_SERVERS} MCP servers"));
        }
        if cancelled.load(Ordering::Acquire) {
            return Err("ACP MCP setup cancelled".into());
        }
        // Settle the host's real registry before checking name collisions.
        // Empty configurations retain composition on the first prompt.
        super::wire_stdio_substrate();
        let config = susi_config::SusiConfig::load_global().unwrap_or_default();
        let handshake = Duration::from_secs(config.cloud_scout_timeout_secs().max(1));
        let tools = SessionTools::connect(spec, cancelled, handshake, &self.processes)?;
        Ok(Some(Arc::new(SessionExecutor {
            base: Arc::clone(&self.base),
            tools: Arc::new(tools),
            lease: Duration::from_secs(config.execution_lease_secs().max(1)),
        })))
    }
}

impl Drop for SessionFactory {
    fn drop(&mut self) {
        // Detached mission workers may still own a revoked scope. Connection
        // teardown nevertheless owns reaping before the CLI process exits.
        let processes = self.processes.get_mut().unwrap_or_else(|e| e.into_inner());
        for process in processes.drain(..).filter_map(|weak| weak.upgrade()) {
            process.finish();
        }
    }
}

struct SessionExecutor {
    base: Arc<GawdAcpExecutor>,
    tools: Arc<SessionTools>,
    lease: Duration,
}

impl AcpTurnExecutor for SessionExecutor {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        // AcpEngine's owned-token entrypoint installs the exact shared token.
        let cancel = susi_config::progress::current_cancel()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(cancelled.load(Ordering::Acquire))));
        let turn = Arc::new(TurnTools {
            session: Arc::clone(&self.tools),
            active: Arc::new(AtomicBool::new(true)),
            cancel: Arc::clone(&cancel),
            deadline: Instant::now() + self.lease,
        });
        struct Finish(Arc<TurnTools>);
        impl Drop for Finish {
            fn drop(&mut self) {
                self.0.active.store(false, Ordering::Release);
            }
        }
        let _finish = Finish(Arc::clone(&turn));
        let scope = ToolScope::new(turn, cancel);
        scoped_tools::with_scope(Some(scope), || self.base.execute_turn(ctx, cancelled, emit))
    }

    fn close_session(&self) {
        self.tools.close();
    }
}

struct ToolTarget {
    server: usize,
    name: String,
}

struct SessionTools {
    admission: AcpAdmissionPolicy,
    servers: Vec<Server>,
    tools: Vec<ScopedTool>,
    targets: BTreeMap<String, ToolTarget>,
    closed: AtomicBool,
}

impl SessionTools {
    fn connect(
        spec: &AcpSessionSpec,
        cancelled: Arc<AtomicBool>,
        handshake: Duration,
        processes: &Mutex<Vec<Weak<Process>>>,
    ) -> Result<Self, String> {
        let mut session = Self {
            admission: spec.admission.clone(),
            servers: Vec::new(),
            tools: Vec::new(),
            targets: BTreeMap::new(),
            closed: AtomicBool::new(false),
        };
        let deadline = Instant::now() + handshake;
        let mut remaining_bytes = MAX_CATALOG_BYTES;
        for (server_index, supplied) in spec.mcp_servers.iter().enumerate() {
            authorize(&session.admission, &supplied.command, None)?;
            let config = susi_tools::McpServerConfig {
                command: supplied.command.clone(),
                args: supplied.args.clone(),
                env: Some(
                    supplied
                        .env
                        .iter()
                        .map(|v| (v.name.clone(), v.value.clone()))
                        .collect(),
                ),
                extra: HashMap::from([("cwd".into(), json!(spec.cwd))]),
            };
            let (server, catalog) = Server::connect(
                &config,
                susi_core::mac_policy::MacPolicy::global(),
                Lease {
                    deadline,
                    cancel: Some(Arc::clone(&cancelled)),
                    turn_cancel: None,
                    active: None,
                },
                processes,
                (MAX_TOOLS - session.tools.len(), remaining_bytes),
            )?;
            session.servers.push(server);
            for (tool_index, tool) in catalog.into_iter().enumerate() {
                // The host counted the same serialized definitions before
                // retaining each page. Carry the budget across all peers.
                remaining_bytes = remaining_bytes
                    .saturating_sub(serde_json::to_vec(&tool).map_err(|e| e.to_string())?.len());
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or("MCP tool has no name")?;
                let schema = tool
                    .get("inputSchema")
                    .filter(|v| v.is_object())
                    .ok_or("MCP tool has no object inputSchema")?;
                // Provider-neutral function names do not embed commands or env.
                let alias = format!("acp_mcp_{server_index}_{tool_index}");
                if susi_tools::ToolRegistry::exists(&alias) {
                    return Err("ACP MCP tool name collides with a host tool".into());
                }
                session.tools.push(ScopedTool {
                    name: alias.clone(),
                    description: format!(
                        "{}/{}: {}",
                        supplied.name,
                        name,
                        tool.get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    ),
                    input_schema: schema.clone(),
                    required_actions: vec![(
                        susi_core::mac_policy::actions::PROCESS_EXEC.into(),
                        supplied.command.clone(),
                    )],
                });
                session.targets.insert(
                    alias,
                    ToolTarget {
                        server: server_index,
                        name: name.into(),
                    },
                );
            }
        }
        if cancelled.load(Ordering::Acquire) {
            return Err("ACP MCP setup cancelled".into());
        }
        Ok(session)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        for server in &self.servers {
            server.io.close();
        }
    }
}

impl Drop for SessionTools {
    fn drop(&mut self) {
        self.close();
    }
}

struct TurnTools {
    session: Arc<SessionTools>,
    active: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}

impl ScopedToolProvider for TurnTools {
    fn tools(&self) -> &[ScopedTool] {
        &self.session.tools
    }

    fn check(&self) -> Result<(), String> {
        if self.session.closed.load(Ordering::Acquire) || !self.active.load(Ordering::Acquire) {
            return Err("ACP MCP session or turn is closed".into());
        }
        if self.cancel.load(Ordering::Acquire) {
            return Err("ACP MCP turn cancelled".into());
        }
        if Instant::now() >= self.deadline {
            return Err("ACP MCP turn lease expired".into());
        }
        Ok(())
    }

    fn call(
        &self,
        name: &str,
        args: &Value,
        _workspace: &Path,
        mission: Option<&str>,
        cancel: Option<susi_config::progress::Cancel>,
        turn_cancel: susi_config::progress::Cancel,
    ) -> Result<String, String> {
        self.check()?;
        let target = self
            .session
            .targets
            .get(name)
            .ok_or("Unknown ACP MCP tool")?;
        let server = &self.session.servers[target.server];
        authorize(&self.session.admission, &server.command, mission)?;
        let result = server.call(
            &target.name,
            args,
            Lease {
                deadline: self.deadline,
                cancel,
                turn_cancel: Some(turn_cancel),
                active: Some(Arc::clone(&self.active)),
            },
        )?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(format!("MCP tool failed: {result}"));
        }
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

fn authorize(
    admission: &AcpAdmissionPolicy,
    command: &str,
    mission: Option<&str>,
) -> Result<(), String> {
    use susi_core::mac_policy::{actions, MacPolicy};
    if !admission
        .policy()
        .is_permitted(&admission.subject, actions::PROCESS_EXEC, command)
    {
        return Err("ACP MCP requires process.exec for its admission subject".into());
    }
    for subject in std::iter::once("susi").chain(mission) {
        if !MacPolicy::global().is_permitted(subject, actions::PROCESS_EXEC, command) {
            return Err("ACP MCP process.exec permission was denied or revoked".into());
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Lease {
    deadline: Instant,
    cancel: Option<Arc<AtomicBool>>,
    turn_cancel: Option<Arc<AtomicBool>>,
    active: Option<Arc<AtomicBool>>,
}

impl Lease {
    fn check(&self) -> io::Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|v| v.load(Ordering::Acquire))
            || self
                .turn_cancel
                .as_ref()
                .is_some_and(|v| v.load(Ordering::Acquire))
            || self
                .active
                .as_ref()
                .is_some_and(|v| !v.load(Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "MCP operation cancelled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP operation lease expired",
            ));
        }
        Ok(())
    }
}

struct Process {
    child: Mutex<Option<(Child, Option<susi_harness::workload_process::Tree>)>>,
    stop: AtomicBool,
    owner: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Process {
    fn own(child: Child, tree: susi_harness::workload_process::Tree) -> io::Result<Arc<Self>> {
        let process = Arc::new(Self {
            child: Mutex::new(Some((child, Some(tree)))),
            stop: AtomicBool::new(false),
            owner: Mutex::new(None),
        });
        let owned = Arc::clone(&process);
        let owner = std::thread::Builder::new()
            .name("acp-mcp-owner".into())
            .spawn(move || loop {
                if owned.stop.load(Ordering::Acquire) {
                    owned.reap();
                    return;
                }
                let mut slot = owned.child.lock().unwrap_or_else(|e| e.into_inner());
                match slot
                    .as_mut()
                    .map(|(child, tree)| susi_harness::workload_process::try_wait(tree, child))
                {
                    None | Some(Ok(Some(_))) => {
                        slot.take();
                        return;
                    }
                    Some(Err(_)) => {
                        drop(slot);
                        owned.reap();
                        return;
                    }
                    Some(Ok(None)) => {}
                }
                drop(slot);
                std::thread::sleep(POLL);
            });
        match owner {
            Ok(owner) => *process.owner.lock().unwrap_or_else(|e| e.into_inner()) = Some(owner),
            Err(error) => {
                process.reap();
                return Err(error);
            }
        }
        Ok(process)
    }

    fn finish(&self) {
        self.stop.store(true, Ordering::Release);
        // Serialize concurrent final owners through completion, not merely
        // through taking the JoinHandle. The child owner never takes this lock.
        let mut owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = owner.take() {
            let _ = handle.join();
        }
    }

    fn reap(&self) {
        let child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some((mut child, tree)) = child {
            // Stop descendants before reaping their group leader. On Windows
            // the owned kill-on-close Job handles even an exited root process.
            drop(tree);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct ManagedIo {
    process: Arc<Process>,
    closed: AtomicBool,
    lease: Mutex<Option<Lease>>,
}

impl ManagedIo {
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.process.stop.store(true, Ordering::Release);
    }

    fn check(&self) -> io::Result<()> {
        let result = self
            .lease
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(Ok(()), Lease::check);
        if result.is_err() {
            self.close();
            return result;
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ACP MCP connection closed",
            ));
        }
        Ok(())
    }
}

struct Server {
    command: String,
    host: Mutex<McpHost>,
    io: Arc<ManagedIo>,
}

impl Server {
    fn connect(
        config: &susi_tools::McpServerConfig,
        policy: &susi_core::mac_policy::MacPolicy,
        lease: Lease,
        processes: &Mutex<Vec<Weak<Process>>>,
        (max_tools, max_bytes): (usize, usize),
    ) -> Result<(Self, Vec<Value>), String> {
        lease.check().map_err(|e| e.to_string())?;
        let mut command = susi_tools::authorized_stdio_command_literal(config, policy)?;
        let (mut child, tree) =
            susi_harness::workload_process::spawn(&mut command).map_err(|e| e.to_string())?;
        let input = child.stdin.take();
        let output = child.stdout.take();
        let process = Process::own(child, tree).map_err(|e| e.to_string())?;
        {
            let mut owners = processes.lock().unwrap_or_else(|e| e.into_inner());
            owners.retain(|process| process.strong_count() > 0);
            owners.push(Arc::downgrade(&process));
        }
        let io = Arc::new(ManagedIo {
            process,
            closed: AtomicBool::new(false),
            lease: Mutex::new(Some(lease)),
        });
        let setup = (|| {
            let input = input.ok_or("MCP child has no stdin")?;
            let output = output.ok_or("MCP child has no stdout")?;
            let reader = PumpReader::start(output, Arc::clone(&io)).map_err(|e| e.to_string())?;
            let writer = PumpWriter::start(input, Arc::clone(&io)).map_err(|e| e.to_string())?;
            let close = Arc::clone(&io);
            let mut host = McpHost::new();
            host.connect_duplex(
                "session",
                DuplexStreams {
                    reader: Box::new(reader),
                    writer: Box::new(writer),
                    on_close: Some(Box::new(move || close.close())),
                },
                HostPolicy::strict("susi-acp", susi::SUSI_VERSION),
            )
            .map_err(|e| e.to_string())?;
            let catalog = host
                .list_tools_bounded("session", max_tools, max_bytes)
                .map_err(|e| e.to_string())?;
            Ok((host, catalog))
        })();
        match setup {
            Ok((host, catalog)) => {
                io.lease.lock().unwrap_or_else(|e| e.into_inner()).take();
                Ok((
                    Self {
                        command: config.command.clone(),
                        host: Mutex::new(host),
                        io,
                    },
                    catalog,
                ))
            }
            Err(error) => {
                io.close();
                io.process.finish();
                Err(error)
            }
        }
    }

    fn call(&self, name: &str, args: &Value, lease: Lease) -> Result<Value, String> {
        let mut host = loop {
            lease.check().map_err(|e| e.to_string())?;
            self.io.check().map_err(|e| e.to_string())?;
            match self.host.try_lock() {
                Ok(host) => break host,
                Err(TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(TryLockError::WouldBlock) => std::thread::sleep(POLL),
            }
        };
        *self.io.lease.lock().unwrap_or_else(|e| e.into_inner()) = Some(lease);
        struct Clear<'a>(&'a ManagedIo);
        impl Drop for Clear<'_> {
            fn drop(&mut self) {
                self.0
                    .lease
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
            }
        }
        let _clear = Clear(&self.io);
        let result = host
            .call_tool("session", name, args, None)
            .map_err(|e| e.to_string());
        if result.is_err() {
            self.io.close();
        }
        result
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.io.close();
        // close_session only signals; final ownership release waits for reaping
        // so the CLI cannot exit while its child owner is still scheduled.
        self.io.process.finish();
    }
}

struct PumpReader {
    receive: mpsc::Receiver<io::Result<Vec<u8>>>,
    bytes: Vec<u8>,
    offset: usize,
    io: Arc<ManagedIo>,
}

impl PumpReader {
    fn start(mut output: impl Read + Send + 'static, io: Arc<ManagedIo>) -> io::Result<Self> {
        let (send, receive) = mpsc::sync_channel(2);
        let state = Arc::clone(&io);
        std::thread::Builder::new()
            .name("acp-mcp-read".into())
            .spawn(move || loop {
                if state.check().is_err() {
                    break;
                }
                let mut bytes = vec![0; 8192];
                let result = output.read(&mut bytes).map(|n| {
                    bytes.truncate(n);
                    bytes
                });
                if result
                    .as_ref()
                    .is_err_and(|e| e.kind() == io::ErrorKind::Interrupted)
                {
                    continue;
                }
                let finished = result.as_ref().map_or(true, Vec::is_empty);
                let mut pending = result;
                loop {
                    if state.check().is_err() {
                        return;
                    }
                    match send.try_send(pending) {
                        Ok(()) => break,
                        Err(mpsc::TrySendError::Full(returned)) => {
                            pending = returned;
                            std::thread::sleep(POLL);
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                    }
                }
                if finished {
                    break;
                }
            })?;
        Ok(Self {
            receive,
            bytes: Vec::new(),
            offset: 0,
            io,
        })
    }
}

impl Read for PumpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bytes = self.fill_buf()?;
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for PumpReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.io.check()?;
        if self.offset == self.bytes.len() {
            loop {
                self.io.check()?;
                match self.receive.recv_timeout(POLL) {
                    Ok(result) => {
                        self.io.check()?;
                        self.bytes = result?;
                        self.offset = 0;
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.io.check()?;
                        return Ok(&[]);
                    }
                }
            }
        }
        Ok(&self.bytes[self.offset..])
    }
    fn consume(&mut self, n: usize) {
        self.offset = self.bytes.len().min(self.offset.saturating_add(n));
    }
}

type WriteJob = (Vec<u8>, mpsc::SyncSender<io::Result<()>>);

struct PumpWriter {
    send: mpsc::SyncSender<WriteJob>,
    io: Arc<ManagedIo>,
}

impl PumpWriter {
    fn start(mut input: impl Write + Send + 'static, io: Arc<ManagedIo>) -> io::Result<Self> {
        let (send, receive) = mpsc::sync_channel::<WriteJob>(1);
        let state = Arc::clone(&io);
        std::thread::Builder::new()
            .name("acp-mcp-write".into())
            .spawn(move || loop {
                if state.check().is_err() {
                    break;
                }
                let (bytes, reply) = match receive.recv_timeout(POLL) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let result = input.write_all(&bytes).and_then(|()| input.flush());
                let failed = result.is_err();
                let _ = reply.send(result);
                if failed {
                    break;
                }
            })?;
        Ok(Self { send, io })
    }
}

impl Write for PumpWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let n = bytes.len().min(8192);
        let (reply, result) = mpsc::sync_channel(1);
        let mut job = (bytes[..n].to_vec(), reply);
        loop {
            self.io.check()?;
            match self.send.try_send(job) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    job = returned;
                    std::thread::sleep(POLL);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.io.check()?;
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "MCP writer stopped",
                    ));
                }
            }
        }
        loop {
            self.io.check()?;
            match result.recv_timeout(POLL) {
                Ok(outcome) => return outcome.map(|()| n),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.io.check()?;
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "MCP writer stopped",
                    ));
                }
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.io.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed(lease: Lease) -> Arc<ManagedIo> {
        Arc::new(ManagedIo {
            process: Arc::new(Process {
                child: Mutex::new(None),
                stop: AtomicBool::new(false),
                owner: Mutex::new(None),
            }),
            closed: AtomicBool::new(false),
            lease: Mutex::new(Some(lease)),
        })
    }

    fn lease() -> Lease {
        Lease {
            deadline: Instant::now() + Duration::from_secs(1),
            cancel: Some(Arc::new(AtomicBool::new(false))),
            turn_cancel: Some(Arc::new(AtomicBool::new(false))),
            active: Some(Arc::new(AtomicBool::new(true))),
        }
    }

    struct Blocking(mpsc::Receiver<()>);
    impl Read for Blocking {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Ok(0)
        }
    }
    impl Write for Blocking {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_read_and_write_obey_deadline_and_stop_child() {
        for reading in [true, false] {
            let mut lease = lease();
            lease.deadline = Instant::now() + Duration::from_millis(50);
            let io = managed(lease);
            let (release, blocked) = mpsc::channel();
            let started = Instant::now();
            let error = if reading {
                PumpReader::start(Blocking(blocked), io.clone())
                    .unwrap()
                    .read(&mut [0; 1])
                    .unwrap_err()
            } else {
                PumpWriter::start(Blocking(blocked), io.clone())
                    .unwrap()
                    .write(&[1])
                    .unwrap_err()
            };
            drop(release);
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(io.process.stop.load(Ordering::Acquire));
        }
    }

    #[test]
    fn blocked_io_observes_child_turn_and_owner_revocation() {
        for which in 0..3 {
            let lease = lease();
            let cancel = match which {
                0 => lease.cancel.as_ref().unwrap(),
                1 => lease.turn_cancel.as_ref().unwrap(),
                _ => lease.active.as_ref().unwrap(),
            }
            .clone();
            let io = managed(lease);
            let (release, blocked) = mpsc::channel();
            let reader = PumpReader::start(Blocking(blocked), io.clone()).unwrap();
            let (done, receive) = mpsc::channel();
            let handle = std::thread::spawn(move || {
                let mut reader = reader;
                // read_line retries Interrupted: cancellation must be terminal,
                // unlike a raw operating-system EINTR handled by the pump.
                let kind = reader.read_line(&mut String::new()).unwrap_err().kind();
                let _ = done.send(kind);
            });
            cancel.store(which == 0 || which == 1, Ordering::Release);
            assert_eq!(
                receive.recv_timeout(Duration::from_secs(1)).unwrap(),
                io::ErrorKind::ConnectionAborted
            );
            handle.join().unwrap();
            drop(release);
            assert!(io.process.stop.load(Ordering::Acquire));
        }
    }

    #[test]
    fn closure_is_nonblocking_even_when_reader_is_blocked() {
        let io = managed(lease());
        let (release, blocked) = mpsc::channel();
        let mut reader = PumpReader::start(Blocking(blocked), io.clone()).unwrap();
        io.close();
        assert_eq!(
            reader.read(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        drop(release);
        assert!(io.process.stop.load(Ordering::Acquire));
    }

    #[test]
    fn interrupted_pipe_read_retries_before_forwarding_data() {
        struct InterruptedOnce(bool);
        impl Read for InterruptedOnce {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let bytes = b"{\"jsonrpc\":\"2.0\"}\n";
                out[..bytes.len()].copy_from_slice(bytes);
                Ok(bytes.len())
            }
        }
        let io = managed(lease());
        let mut reader = PumpReader::start(InterruptedOnce(false), io.clone()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"jsonrpc\":\"2.0\"}\n");
        io.close();
    }

    #[test]
    fn closed_pumps_release_idle_writer_and_full_reader_queue() {
        struct Observed(Arc<AtomicBool>);
        impl Drop for Observed {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        impl Read for Observed {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                out.fill(b'x');
                Ok(out.len())
            }
        }
        impl Write for Observed {
            fn write(&mut self, out: &[u8]) -> io::Result<usize> {
                Ok(out.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let io = managed(lease());
        let reader_dropped = Arc::new(AtomicBool::new(false));
        let writer_dropped = Arc::new(AtomicBool::new(false));
        let _reader = PumpReader::start(Observed(reader_dropped.clone()), io.clone()).unwrap();
        let _writer = PumpWriter::start(Observed(writer_dropped.clone()), io.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        io.close();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !reader_dropped.load(Ordering::Acquire) || !writer_dropped.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "closed pump retained its pipe");
            std::thread::sleep(POLL);
        }
    }
}
