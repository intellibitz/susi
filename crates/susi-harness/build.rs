//! The standalone harness uses the same native Rust entry as Susi.
#![forbid(unsafe_code)]

fn main() -> std::io::Result<()> {
    susi_build_os_ucrt::configure_for(&["susi-harness"])
}
