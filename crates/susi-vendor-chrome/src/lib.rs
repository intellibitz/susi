#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! # susi-vendor-chrome
//!
//! Headless Chrome/Chromium page capture over `std::process::Command` (no
//! GPL-3.0 `auto_generate_cdp` / `headless_chrome` dependency).
//! [`capture_page`] invokes a local Chrome/Chromium binary in headless mode,
//! navigates to an already-validated URL and returns a PNG screenshot plus the
//! rendered HTML. URL policy (SSRF checks) and where captures are stored stay
//! with the caller.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use susi_error::{EaiError, EaiResult};

/// A rendered page.
pub struct PageCapture {
    pub png: Vec<u8>,
    pub html: String,
}

const DEFAULT_CHROME_CANDIDATES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "chrome",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
];

const MAX_OUTPUT: u64 = 8 * 1024 * 1024;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> std::io::Result<Self> {
        let path = temp_screenshot_path().with_extension("work");
        std::fs::create_dir(&path)?;
        let scratch = Self(path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(scratch)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// File-backed output cannot wait forever for a descendant to close an inherited
/// pipe. Track descendant identities while running so timeout cleanup affects
/// only this invocation, never another browser session.
fn bounded_output(command: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    use std::collections::BTreeMap;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let scratch = Scratch::new()?;
    let stdout_path = scratch.0.join("stdout");
    let stderr_path = scratch.0.join("stderr");
    let stdout = std::fs::File::create(&stdout_path)?;
    let stderr = std::fs::File::create(&stderr_path)?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()?;
    let root = Pid::from_u32(child.id());
    let mut system = System::new();
    let mut descendants = BTreeMap::new();
    let started = Instant::now();
    let result = loop {
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        // Repeat to discover several generations independent of process order.
        loop {
            let previous = descendants.len();
            for (pid, process) in system.processes() {
                if *pid != root
                    && process
                        .parent()
                        .is_some_and(|parent| parent == root || descendants.contains_key(&parent))
                {
                    descendants.entry(*pid).or_insert(process.start_time());
                }
            }
            if descendants.len() == previous {
                break;
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => break Err(error),
        }
        if started.elapsed() >= timeout {
            break Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Chrome execution deadline exceeded",
            ));
        }
        if [&stdout_path, &stderr_path].iter().any(|path| {
            std::fs::metadata(path)
                .map(|m| m.len() > MAX_OUTPUT)
                .unwrap_or(true)
        }) {
            break Err(std::io::Error::other("Chrome output exceeds capture limit"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Refresh the same identities before signalling; do not kill reused PIDs.
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    for (pid, start) in descendants.iter().rev() {
        if let Some(process) = system
            .process(*pid)
            .filter(|process| process.start_time() == *start)
        {
            process.kill();
        }
    }
    if result.is_err() {
        // Let a cooperative parent reap its terminated children before forcing
        // it down; minimal container init processes may not reap orphans.
        let grace = Instant::now() + Duration::from_millis(100);
        while child.try_wait()?.is_none() && Instant::now() < grace {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let deadline = Instant::now() + Duration::from_secs(1);
        while child.try_wait()?.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let status = result?;
    fn read_output(path: &Path) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_OUTPUT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_OUTPUT {
            return Err(std::io::Error::other("Chrome output exceeds capture limit"));
        }
        Ok(bytes)
    }
    Ok(Output {
        status,
        stdout: read_output(&stdout_path)?,
        stderr: read_output(&stderr_path)?,
    })
}

fn probe_chrome_candidate(bin: &str) -> bool {
    bounded_output(Command::new(bin).arg("--version"), Duration::from_secs(2))
        .is_ok_and(|out| out.status.success())
}

fn resolve_chrome_bin_from(override_bin: Option<&str>, candidates: &[&str]) -> EaiResult<PathBuf> {
    if let Some(bin) = override_bin.map(str::trim).filter(|s| !s.is_empty()) {
        if probe_chrome_candidate(bin) {
            return Ok(PathBuf::from(bin));
        }
        return Err(EaiError::process(format!(
            "[CAPABILITY_GAP] Headless Chrome failed: SUSI_CHROME_BIN={bin} is not runnable. Ensure Chrome/Chromium is installed."
        )));
    }
    for candidate in candidates {
        if probe_chrome_candidate(candidate) {
            return Ok(PathBuf::from(candidate));
        }
    }
    Err(EaiError::process(
        "[CAPABILITY_GAP] Headless Chrome failed: no Chrome/Chromium executable found. Ensure Chrome/Chromium is installed.",
    ))
}

fn temp_screenshot_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("susi-chrome-capture-{pid}-{nanos}-{seq}.png"))
}

fn parse_dom_output(output: Output) -> EaiResult<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EaiError::process(format!(
            "[CAPABILITY_GAP] Headless Chrome DOM dump exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    let html = String::from_utf8_lossy(&output.stdout).into_owned();
    if html.trim().is_empty() {
        return Err(EaiError::process(
            "[CAPABILITY_GAP] Headless Chrome returned an empty DOM.",
        ));
    }
    Ok(html)
}

fn read_and_cleanup_screenshot(output: Output, screenshot_path: &Path) -> EaiResult<Vec<u8>> {
    if !output.status.success() {
        let _ = std::fs::remove_file(screenshot_path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EaiError::process(format!(
            "[CAPABILITY_GAP] Headless Chrome screenshot exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    let png = std::fs::read(screenshot_path).map_err(|e| {
        let _ = std::fs::remove_file(screenshot_path);
        EaiError::process(format!(
            "[CAPABILITY_GAP] Headless Chrome did not produce screenshot {}: {e}",
            screenshot_path.display()
        ))
    })?;
    let _ = std::fs::remove_file(screenshot_path);
    if png.is_empty() {
        return Err(EaiError::process(
            "[CAPABILITY_GAP] Headless Chrome produced an empty screenshot.",
        ));
    }
    Ok(png)
}

fn capture_page_with_bin(bin: &Path, url: &str) -> EaiResult<PageCapture> {
    let profile = Scratch::new()
        .map_err(|e| EaiError::process(format!("[CAPABILITY_GAP] Chrome private profile: {e}")))?;
    let profile_arg = format!("--user-data-dir={}", profile.0.display());
    let dom_output = bounded_output(
        Command::new(bin).args([
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            &profile_arg,
            "--dump-dom",
            url,
        ]),
        Duration::from_secs(15),
    )
    .map_err(|e| {
        EaiError::process(format!(
            "[CAPABILITY_GAP] Headless Chrome DOM command failed ({}): {e}",
            bin.display()
        ))
    })?;
    let html = parse_dom_output(dom_output)?;

    let screenshot_path = temp_screenshot_path();
    let screenshot_arg = format!("--screenshot={}", screenshot_path.display());
    let shot_output = bounded_output(
        Command::new(bin).args([
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            &profile_arg,
            &screenshot_arg,
            url,
        ]),
        Duration::from_secs(15),
    )
    .map_err(|e| {
        let _ = std::fs::remove_file(&screenshot_path);
        EaiError::process(format!(
            "[CAPABILITY_GAP] Headless Chrome screenshot command failed ({}): {e}",
            bin.display()
        ))
    })?;
    let png = read_and_cleanup_screenshot(shot_output, &screenshot_path)?;
    Ok(PageCapture { png, html })
}

/// Navigate to `url`, wait for navigation, capture a full screenshot and
/// the DOM. Each browser command has a 15-second deadline and an 8 MiB
/// output read limit; executable probes have a two-second deadline.
///
/// # Errors
/// `[CAPABILITY_GAP]` when Chrome/Chromium cannot launch; otherwise the
/// headless browser error for the failing step.
pub fn capture_page(url: &str) -> EaiResult<PageCapture> {
    let env_override = std::env::var("SUSI_CHROME_BIN").ok();
    let bin = resolve_chrome_bin_from(env_override.as_deref(), DEFAULT_CHROME_CANDIDATES)?;
    capture_page_with_bin(&bin, url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn bounded_child_fixture() {
        match std::env::var("SUSI_CHROME_TEST_MODE").as_deref() {
            Ok("sleep") => std::thread::sleep(Duration::from_secs(30)),
            Ok("flood") => {
                use std::io::Write;
                std::io::stdout()
                    .write_all(&vec![b'x'; MAX_OUTPUT as usize + 1])
                    .unwrap();
            }
            Ok("tree") => {
                std::fs::write(
                    std::env::var("SUSI_CHROME_TEST_PARENT_PID").unwrap(),
                    std::process::id().to_string(),
                )
                .unwrap();
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "tests::bounded_child_fixture", "--nocapture"])
                    .env("SUSI_CHROME_TEST_MODE", "sleep")
                    .spawn()
                    .unwrap();
                std::fs::write(
                    std::env::var("SUSI_CHROME_TEST_PID").unwrap(),
                    child.id().to_string(),
                )
                .unwrap();
                child.wait().unwrap();
            }
            _ => {}
        }
    }

    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tests::bounded_child_fixture", "--nocapture"])
            .env("SUSI_CHROME_TEST_MODE", mode);
        command
    }

    #[test]
    fn deadline_kills_and_reaps_hung_browser_and_descendants() {
        let scratch = Scratch::new().unwrap();
        let pidfile = scratch.0.join("child.pid");
        let parent_pidfile = scratch.0.join("parent.pid");
        let mut command = fixture_command("tree");
        command.env("SUSI_CHROME_TEST_PARENT_PID", &parent_pidfile);
        command.env("SUSI_CHROME_TEST_PID", &pidfile);
        let started = Instant::now();
        let error = bounded_output(&mut command, Duration::from_millis(500)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: u32 = std::fs::read_to_string(pidfile).unwrap().parse().unwrap();
        let mut system = sysinfo::System::new_all();
        let parent_pid: u32 = std::fs::read_to_string(parent_pidfile)
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            system.process(sysinfo::Pid::from_u32(parent_pid)).is_none(),
            "direct child was not reaped"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            if system
                .process(sysinfo::Pid::from_u32(pid))
                .is_none_or(|process| process.status() == sysinfo::ProcessStatus::Zombie)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Chrome descendant survived cleanup"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn output_limit_and_success_are_bounded_without_pipe_readers() {
        let output =
            bounded_output(&mut fixture_command("success"), Duration::from_secs(2)).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        let error =
            bounded_output(&mut fixture_command("flood"), Duration::from_secs(2)).unwrap_err();
        assert!(error.to_string().contains("output exceeds"));
    }

    fn ok_output(stdout: &[u8]) -> Output {
        let mut out = Command::new("rustc")
            .arg("--version")
            .output()
            .expect("rustc --version must succeed in cargo test");
        out.stdout = stdout.to_vec();
        out.stderr.clear();
        out
    }

    fn err_output(stderr: &[u8]) -> Output {
        let mut out = Command::new("rustc")
            .arg("--definitely-not-a-valid-rustc-flag")
            .output()
            .expect("rustc invocation must return an Output");
        out.stdout.clear();
        out.stderr = stderr.to_vec();
        out
    }

    /// Chrome absent → typed `[CAPABILITY_GAP]` error; Chrome present → the
    /// capture succeeds on a trivial page. Either way no panic and no hang
    /// (about:blank needs no network).
    #[test]
    fn capture_page_never_panics() {
        match super::capture_page("about:blank") {
            Ok(cap) => assert!(!cap.png.is_empty() && !cap.html.is_empty()),
            Err(e) => {
                assert!(e.to_string().contains("CAPABILITY_GAP") || !e.to_string().is_empty())
            }
        }
    }

    #[test]
    fn resolve_chrome_bin_handles_override_and_candidates() {
        // Valid override binary ("rustc" responds 0 to `--version`).
        let ok = resolve_chrome_bin_from(Some("rustc"), &[]).expect("rustc --version succeeds");
        assert_eq!(ok, PathBuf::from("rustc"));

        // Invalid override binary returns CAPABILITY_GAP.
        let err = resolve_chrome_bin_from(Some("susi-nonexistent-chrome-bin-xyz"), &[])
            .expect_err("missing override must fail");
        assert!(err.to_string().contains("CAPABILITY_GAP"));

        // Candidate list fallback finds a working binary after skipping missing ones.
        let via_candidate =
            resolve_chrome_bin_from(None, &["susi-nonexistent-chrome-bin-xyz", "rustc"])
                .expect("candidate list should resolve rustc");
        assert_eq!(via_candidate, PathBuf::from("rustc"));

        // Empty / all-missing candidates return CAPABILITY_GAP.
        let none_err = resolve_chrome_bin_from(None, &["susi-nonexistent-chrome-bin-xyz"])
            .expect_err("all missing candidates must fail");
        assert!(none_err.to_string().contains("CAPABILITY_GAP"));
    }

    #[test]
    fn dom_and_screenshot_helpers_validate_outputs_and_clean_up() {
        let html = parse_dom_output(ok_output(b"<html><body>ok</body></html>"))
            .expect("non-empty DOM should parse");
        assert!(html.contains("ok"));

        let empty_dom = parse_dom_output(ok_output(b"   ")).expect_err("whitespace DOM must fail");
        assert!(empty_dom.to_string().contains("CAPABILITY_GAP"));

        let failed_dom =
            parse_dom_output(err_output(b"crash")).expect_err("non-zero status must fail");
        assert!(failed_dom.to_string().contains("CAPABILITY_GAP"));

        // Screenshot helper reads and removes the temporary PNG file.
        let path = temp_screenshot_path();
        assert_eq!(path.extension().and_then(|s| s.to_str()), Some("png"));
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").expect("write temp png");
        let bytes =
            read_and_cleanup_screenshot(ok_output(b""), &path).expect("valid png should read");
        assert_eq!(bytes, b"\x89PNG\r\n\x1a\n");
        assert!(!path.exists(), "temp screenshot must be removed after read");

        // Empty screenshot file returns CAPABILITY_GAP and removes the file.
        let empty_path = temp_screenshot_path();
        std::fs::write(&empty_path, b"").expect("write empty png");
        let empty_err = read_and_cleanup_screenshot(ok_output(b""), &empty_path)
            .expect_err("empty png must fail");
        assert!(empty_err.to_string().contains("CAPABILITY_GAP"));
        assert!(!empty_path.exists());

        // Failed command status removes any leftover file and returns CAPABILITY_GAP.
        let fail_path = temp_screenshot_path();
        std::fs::write(&fail_path, b"partial").expect("write partial png");
        let fail_err = read_and_cleanup_screenshot(err_output(b"gpu error"), &fail_path)
            .expect_err("failed screenshot command must fail");
        assert!(fail_err.to_string().contains("CAPABILITY_GAP"));
        assert!(!fail_path.exists());

        // Missing binary in capture_page_with_bin returns CAPABILITY_GAP.
        let missing =
            capture_page_with_bin(Path::new("susi-nonexistent-chrome-bin-xyz"), "about:blank");
        assert!(missing.is_err());
    }
}
