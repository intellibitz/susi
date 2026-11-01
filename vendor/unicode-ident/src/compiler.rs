// SPDX-License-Identifier: Apache-2.0
// Table-free, bounded compiler-backed Unicode identifier service.
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PINNED_VERSION: &str = "1.99.0";
const QUEUE_SIZE: usize = 32;
const CACHE_SIZE: usize = 4096;
const IDLE: Duration = Duration::from_secs(10);
const START_TIMEOUT: Duration = Duration::from_secs(10);
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerError {
    Unavailable,
    WrongVersion,
    Busy,
    Timeout,
    CompileFailed,
    Protocol,
    Io(String),
}
impl std::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "identifier compiler service: {self:?}")
    }
}
impl std::error::Error for CompilerError {}
impl From<std::io::Error> for CompilerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

struct Query {
    ch: char,
    continuation: bool,
    deadline: Instant,
    reply: mpsc::SyncSender<Result<bool, CompilerError>>,
}
enum Request {
    Query(Query),
    Shutdown(mpsc::SyncSender<()>),
}
type Dispatch = Mutex<Option<mpsc::SyncSender<Request>>>;
static DISPATCH: OnceLock<Dispatch> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static STARTS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static ACTIVE: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static MAX_ACTIVE: AtomicU64 = AtomicU64::new(0);

/// Exact identifier acceptance by the pinned installed compiler. Infrastructure
/// errors remain errors: callers must not store them as negative classifications.
pub fn classify(ch: char, continuation: bool) -> Result<bool, CompilerError> {
    let (reply, receive) = mpsc::sync_channel(1);
    let request = Request::Query(Query {
        ch,
        continuation,
        deadline: Instant::now() + CALL_TIMEOUT,
        reply,
    });
    dispatch(request)?;
    receive
        .recv_timeout(CALL_TIMEOUT)
        .map_err(|_| CompilerError::Timeout)?
}

/// Stop the worker and clean its exact owned files. A future call starts a new
/// worker. Shutdown is serialized through the same bounded request queue.
pub fn shutdown() -> Result<(), CompilerError> {
    let (reply, receive) = mpsc::sync_channel(1);
    let mut guard = DISPATCH
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(sender) = guard.as_ref() else {
        return Ok(());
    };
    match sender.try_send(Request::Shutdown(reply)) {
        Ok(()) => {
            receive
                .recv_timeout(CALL_TIMEOUT)
                .map_err(|_| CompilerError::Timeout)?;
            *guard = None;
            Ok(())
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            *guard = None;
            Ok(())
        }
        Err(mpsc::TrySendError::Full(_)) => Err(CompilerError::Busy),
    }
}

fn dispatch(mut request: Request) -> Result<(), CompilerError> {
    let mut guard = DISPATCH
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    loop {
        if guard.is_none() {
            let (sender, receiver) = mpsc::sync_channel(QUEUE_SIZE);
            std::thread::Builder::new()
                .name("susi-ident-compiler".into())
                .spawn(move || worker(receiver))?;
            *guard = Some(sender);
        }
        match guard
            .as_ref()
            .ok_or(CompilerError::Protocol)?
            .try_send(request)
        {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(_)) => return Err(CompilerError::Busy),
            Err(mpsc::TrySendError::Disconnected(returned)) => {
                *guard = None;
                request = returned;
            }
        }
    }
}

fn worker(receiver: mpsc::Receiver<Request>) {
    let mut broker: Option<Broker> = None;
    let mut cache = BTreeMap::new();
    let mut order = VecDeque::new();
    while let Ok(request) = receiver.recv_timeout(IDLE) {
        let query = match request {
            Request::Query(query) => query,
            Request::Shutdown(reply) => {
                drop(broker);
                let _ = reply.send(());
                return;
            }
        };
        if Instant::now() >= query.deadline {
            let _ = query.reply.try_send(Err(CompilerError::Timeout));
            continue;
        }
        let key = (query.ch, query.continuation);
        if let Some(&value) = cache.get(&key) {
            let _ = query.reply.try_send(Ok(value));
            continue;
        }
        let result = (|| {
            if broker.is_none() {
                broker = Some(Broker::start(&resolve_compiler()?)?);
            }
            broker
                .as_mut()
                .ok_or(CompilerError::Protocol)?
                .query(query.ch, query.continuation)
        })();
        if let Ok(value) = &result {
            if cache.len() == CACHE_SIZE {
                if let Some(old) = order.pop_front() {
                    cache.remove(&old);
                }
            }
            cache.insert(key, *value);
            order.push_back(key);
        } else {
            // Failed/expired transport must not misalign subsequent responses.
            // Drop kills and reaps the process before any replacement starts.
            broker = None;
        }
        let _ = query.reply.try_send(result);
    }
    // Idle expiration drops the compiler and files. The dispatcher detects its
    // stale sender and lazily starts another worker; no persistent table exists.
}

fn resolve_compiler() -> Result<PathBuf, CompilerError> {
    if let Some(path) = std::env::var_os("RUSTC").map(PathBuf::from) {
        if let Some(path) = concrete_compiler(&path) {
            return Ok(path);
        }
    }
    let root = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|p| PathBuf::from(p).join(".rustup"))
        })
        .ok_or(CompilerError::Unavailable)?;
    let target = if cfg!(target_os = "windows") {
        format!("{}-pc-windows-msvc", std::env::consts::ARCH)
    } else if cfg!(target_os = "macos") {
        format!("{}-apple-darwin", std::env::consts::ARCH)
    } else {
        format!("{}-unknown-linux-gnu", std::env::consts::ARCH)
    };
    let path = root
        .join("toolchains")
        .join(format!("{PINNED_VERSION}-{target}"))
        .join("bin")
        .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    concrete_compiler(&path).ok_or(CompilerError::Unavailable)
}

fn concrete_compiler(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let path = path.canonicalize().ok()?;
    if !path.is_file() {
        return None;
    }
    let root = path.parent()?.parent()?;
    // Rustup shims do not have an adjacent toolchain library tree. Never execute
    // PATH lookup or rustup, and never install anything during classification.
    root.join("lib/rustlib").is_dir().then_some(path)
}

fn command(compiler: &Path, directory: &Path) -> Command {
    let mut command = Command::new(compiler);
    command
        .current_dir(directory)
        .env_clear()
        .env("RUSTUP_AUTO_INSTALL", "0");
    for name in [
        "SystemRoot",
        "WINDIR",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
        "SYSTEMDRIVE",
        "PROCESSOR_ARCHITECTURE",
        "PROCESSOR_ARCHITEW6432",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("TEMP", directory)
        .env("TMP", directory)
        .env("TMPDIR", directory);
    let mut paths: Vec<PathBuf> = compiler
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    if cfg!(windows) {
        if let Some(root) = std::env::var_os("SystemRoot") {
            paths.push(PathBuf::from(root).join("System32"));
        }
    } else {
        paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    }
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    #[cfg(test)]
    command.stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

struct Files {
    directory: PathBuf,
}
impl Files {
    fn create() -> Result<Self, CompilerError> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "susi-ident-broker-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory)?;
        Ok(Self { directory })
    }
    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        for name in [
            "helper.rs",
            "consumer.rs",
            "helper.dll",
            "helper.pdb",
            "helper.dll.lib",
            "helper.dll.exp",
            "helper.so",
            "helper.dylib",
            "consumer.rmeta",
            "version.txt",
        ] {
            let _ = std::fs::remove_file(self.path(name));
        }
        // Never recursively delete. Unexpected files are preserved.
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn wait(child: &mut Child, limit: Duration) -> Result<(), CompilerError> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(CompilerError::CompileFailed)
                }
            }
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(5)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return match result {
                    Err(error) => Err(error.into()),
                    _ => Err(CompilerError::Timeout),
                };
            }
        }
    }
}

struct Broker {
    child: Child,
    input: Option<ChildStdin>,
    answers: mpsc::Receiver<Result<bool, CompilerError>>,
    reader: Option<std::thread::JoinHandle<()>>,
    _files: Files,
}
impl Broker {
    fn start(compiler: &Path) -> Result<Self, CompilerError> {
        Self::start_with_helper(compiler, HELPER)
    }

    fn start_with_helper(compiler: &Path, helper: &str) -> Result<Self, CompilerError> {
        if !compiler.is_file() {
            return Err(CompilerError::Unavailable);
        }
        let started = Instant::now();
        let files = Files::create()?;
        // Capture a bounded, owned file instead of inheriting output or allowing
        // a pipe to fill. The executable is a concrete installed toolchain binary.
        let version = std::fs::File::create(files.path("version.txt"))?;
        let mut child = command(compiler, &files.directory)
            .arg("--version")
            .stdout(version)
            .spawn()?;
        wait(&mut child, START_TIMEOUT.saturating_sub(started.elapsed()))?;
        let mut text = String::new();
        std::fs::File::open(files.path("version.txt"))?
            .take(256)
            .read_to_string(&mut text)?;
        if !text.starts_with(&format!("rustc {PINNED_VERSION} ")) {
            return Err(CompilerError::WrongVersion);
        }
        std::fs::write(files.path("helper.rs"), helper)?;
        std::fs::write(
            files.path("consumer.rs"),
            "extern crate susi_identifier_broker; susi_identifier_broker::serve!();",
        )?;
        let library = files.path(if cfg!(windows) {
            "helper.dll"
        } else if cfg!(target_os = "macos") {
            "helper.dylib"
        } else {
            "helper.so"
        });
        let mut child = command(compiler, &files.directory)
            .args([
                "--crate-name=susi_identifier_broker",
                "--crate-type=proc-macro",
                "--edition=2021",
                "-Cdebuginfo=0",
                "--cap-lints=allow",
            ])
            .arg(files.path("helper.rs"))
            .arg("-o")
            .arg(&library)
            .spawn()?;
        wait(&mut child, START_TIMEOUT.saturating_sub(started.elapsed()))?;
        let mut child = command(compiler, &files.directory)
            .args([
                "--crate-name=susi_identifier_consumer",
                "--crate-type=lib",
                "--edition=2021",
                "--emit=metadata",
                "--cap-lints=allow",
                "--extern",
            ])
            .arg(format!("susi_identifier_broker={}", library.display()))
            .arg(files.path("consumer.rs"))
            .arg("-o")
            .arg(files.path("consumer.rmeta"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take();
        let mut output = child.stdout.take().ok_or(CompilerError::Protocol)?;
        let (send, answers) = mpsc::sync_channel(1);
        let reader = match std::thread::Builder::new()
            .name("susi-ident-answers".into())
            .spawn(move || loop {
                let mut byte = [0];
                let value = match output.read_exact(&mut byte) {
                    Ok(()) => match byte[0] {
                        0 => Ok(false),
                        1 => Ok(true),
                        _ => Err(CompilerError::Protocol),
                    },
                    Err(_) => Err(CompilerError::Protocol),
                };
                let stop = value.is_err();
                if send.try_send(value).is_err() || stop {
                    break;
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        #[cfg(test)]
        {
            STARTS.fetch_add(1, Ordering::SeqCst);
            let active = ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
            MAX_ACTIVE.fetch_max(active, Ordering::SeqCst);
        }
        Ok(Self {
            child,
            input,
            answers,
            reader: Some(reader),
            _files: files,
        })
    }

    fn query(&mut self, ch: char, continuation: bool) -> Result<bool, CompilerError> {
        let mut message = [0; 5];
        message[..4].copy_from_slice(&(ch as u32).to_le_bytes());
        message[4] = u8::from(continuation);
        self.input
            .as_mut()
            .ok_or(CompilerError::Protocol)?
            .write_all(&message)?;
        self.answers
            .recv_timeout(QUERY_TIMEOUT)
            .map_err(|_| CompilerError::Timeout)?
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        #[cfg(test)]
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
        // `files` drops after process termination, so DLLs are no longer locked.
    }
}

const HELPER: &str = r#"
extern crate proc_macro;
use proc_macro::{Ident, Span, TokenStream};
use std::io::{Read, Write};
#[proc_macro]
pub fn serve(_: TokenStream) -> TokenStream {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut message = [0u8; 5];
        if input.read_exact(&mut message).is_err() { break; }
        let Some(ch) = char::from_u32(u32::from_le_bytes([message[0], message[1], message[2], message[3]])) else { break; };
        if message[4] > 1 { break; }
        let continuation = message[4] == 1;
        let value = if ch == '_' && !continuation { false } else {
            let text = if continuation { format!("a{ch}a") } else { ch.to_string() };
            std::panic::catch_unwind(|| Ident::new(&text, Span::call_site())).is_ok()
        };
        if output.write_all(&[u8::from(value)]).and_then(|_| output.flush()).is_err() { break; }
    }
    TokenStream::new()
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_compiler_service_ordinary_future_concurrent_and_churn() {
        let compiler = resolve_compiler().unwrap();
        let mut broker = Broker::start(&compiler).unwrap();
        let directory = broker._files.directory.clone();
        let pid = broker.child.id();
        for (ch, cont, expected) in [
            ('λ', false, true),
            ('9', false, false),
            ('9', true, true),
            ('_', false, false),
            ('\u{105c0}', false, true),
            ('\u{301}', false, false),
            ('\u{301}', true, true),
            ('\u{feff}', true, false),
            ('😀', true, false),
        ] {
            assert_eq!(broker.query(ch, cont), Ok(expected), "{ch:?}/{cont}");
        }
        for n in 0x40000..0x41400 {
            assert_eq!(broker.query(char::from_u32(n).unwrap(), false), Ok(false));
        }
        assert_eq!(
            broker.child.id(),
            pid,
            "churn must reuse one compiler process"
        );
        drop(broker);
        assert!(
            !directory.exists(),
            "owned files must be cleaned: {directory:?}"
        );
        let starts = STARTS.load(Ordering::SeqCst);
        let mut threads = Vec::new();
        for _ in 0..12 {
            threads.push(std::thread::spawn(|| {
                for _ in 0..20 {
                    assert_eq!(classify('\u{105c0}', false), Ok(true));
                    assert_eq!(classify('\u{feff}', true), Ok(false));
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        for n in 0x40000..0x41400 {
            assert_eq!(classify(char::from_u32(n).unwrap(), false), Ok(false));
        }
        assert_eq!(
            classify('\u{105c0}', false),
            Ok(true),
            "valid results survive cache churn"
        );
        assert_eq!(
            STARTS.load(Ordering::SeqCst),
            starts + 1,
            "concurrency/churn must not create a child per request"
        );
        assert_eq!(MAX_ACTIVE.load(Ordering::SeqCst), 1);
        std::thread::sleep(IDLE + Duration::from_millis(150));
        assert_eq!(
            ACTIVE.load(Ordering::SeqCst),
            0,
            "idle worker must reap its compiler"
        );
        assert_eq!(
            classify('\u{105c0}', false),
            Ok(true),
            "lazy restart after idle"
        );
        assert_eq!(STARTS.load(Ordering::SeqCst), starts + 2);
        shutdown().unwrap();
        assert_eq!(ACTIVE.load(Ordering::SeqCst), 0);

        // A burst beyond queue capacity produces explicit overload errors and
        // never launches additional compiler workers.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(80));
        let mut threads = Vec::new();
        for n in 0..80 {
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                classify(char::from_u32(0x40000 + n).unwrap(), false)
            }));
        }
        let mut accepted = 0;
        let mut busy = 0;
        for thread in threads {
            match thread.join().unwrap() {
                Ok(false) => accepted += 1,
                Err(CompilerError::Busy) => busy += 1,
                result => panic!("unexpected saturation result {result:?}"),
            }
        }
        assert!(
            accepted > 0 && busy > 0,
            "burst must exercise bounded queue"
        );
        assert_eq!(MAX_ACTIVE.load(Ordering::SeqCst), 1);
        shutdown().unwrap();

        // Deliberately stalled real compiler macro: a timeout is infrastructure
        // failure, not a false lexical result; process and files still disappear.
        let stalled = HELPER.replacen(
            "    loop {",
            "    std::thread::sleep(std::time::Duration::from_secs(30));\n    loop {",
            1,
        );
        let mut broker = Broker::start_with_helper(&compiler, &stalled).unwrap();
        let directory = broker._files.directory.clone();
        let started = Instant::now();
        assert_eq!(
            broker.query('\u{105c0}', false),
            Err(CompilerError::Timeout)
        );
        assert!(started.elapsed() < QUERY_TIMEOUT + Duration::from_secs(1));
        drop(broker);
        assert!(!directory.exists());
        assert_eq!(
            classify('\u{105c0}', false),
            Ok(true),
            "timeout cannot become cached false"
        );
        shutdown().unwrap();
    }

    #[test]
    fn missing_compiler_and_shims_are_not_invalid_identifier_results() {
        assert!(matches!(
            Broker::start(Path::new("Z:/definitely-missing-susi-rustc.exe")),
            Err(CompilerError::Unavailable)
        ));
        assert_eq!(concrete_compiler(Path::new("rustc")), None);
    }
}
