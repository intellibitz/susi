#![forbid(unsafe_code)]

//! # susi-vendor-chrome
//!
//! The one crate that links `headless_chrome`. [`capture_page`] launches a
//! headless Chrome/Chromium, navigates to an already-validated URL and
//! returns a PNG screenshot plus the rendered HTML. URL policy (SSRF
//! checks) and where captures are stored stay with the caller.

use susi_error::{EaiError, EaiResult};

/// A rendered page.
pub struct PageCapture {
    pub png: Vec<u8>,
    pub html: String,
}

/// Navigate to `url`, wait for navigation, capture a full screenshot and
/// the DOM.
///
/// # Errors
/// `[CAPABILITY_GAP]` when Chrome/Chromium cannot launch; otherwise the
/// CDP error for the failing step.
pub fn capture_page(url: &str) -> EaiResult<PageCapture> {
    let browser = headless_chrome::Browser::default().map_err(|e| {
        EaiError::process(format!(
            "[CAPABILITY_GAP] Headless Chrome failed: {e}. Ensure Chrome/Chromium is installed."
        ))
    })?;
    let tab = browser
        .new_tab()
        .map_err(|e| EaiError::process(e.to_string()))?;
    tab.navigate_to(url)
        .map_err(|e| EaiError::process(e.to_string()))?;
    tab.wait_until_navigated()
        .map_err(|e| EaiError::process(e.to_string()))?;
    let png = tab
        .capture_screenshot(
            headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
            None,
            None,
            true,
        )
        .map_err(|e| EaiError::process(e.to_string()))?;
    let html = tab
        .get_content()
        .map_err(|e| EaiError::process(e.to_string()))?;
    Ok(PageCapture { png, html })
}
