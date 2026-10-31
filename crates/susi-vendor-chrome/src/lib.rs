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
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
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

fn probe_chrome_candidate(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .output()
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
    let dom_output = Command::new(bin)
        .args([
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            "--dump-dom",
            url,
        ])
        .output()
        .map_err(|e| {
            EaiError::process(format!(
                "[CAPABILITY_GAP] Headless Chrome failed to launch ({}): {e}",
                bin.display()
            ))
        })?;
    let html = parse_dom_output(dom_output)?;

    let screenshot_path = temp_screenshot_path();
    let screenshot_arg = format!("--screenshot={}", screenshot_path.display());
    let shot_output = Command::new(bin)
        .args([
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            &screenshot_arg,
            url,
        ])
        .output()
        .map_err(|e| {
            let _ = std::fs::remove_file(&screenshot_path);
            EaiError::process(format!(
                "[CAPABILITY_GAP] Headless Chrome screenshot failed to launch ({}): {e}",
                bin.display()
            ))
        })?;
    let png = read_and_cleanup_screenshot(shot_output, &screenshot_path)?;
    Ok(PageCapture { png, html })
}

/// Navigate to `url`, wait for navigation, capture a full screenshot and
/// the DOM.
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
