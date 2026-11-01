#![allow(dead_code)]
#[path = "bootstrap/os_ucrt.rs"]
mod os_ucrt;
fn main() -> std::io::Result<()> {
    os_ucrt::configure_for(&[
        "xtask",
        "local-workflow",
        "local-release",
        "ci-release-only",
        "susi-dev",
    ])
}
