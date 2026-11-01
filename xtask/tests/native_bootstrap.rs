// The Cargo-built production bootstrap uses the native Rust process boundary.
// Import its regression tests into Cargo's regular test gate as well.
#[allow(dead_code)] // The standalone main is exercised outside this test harness.
#[path = "../bootstrap/susi-dev.rs"]
mod bootstrap;
