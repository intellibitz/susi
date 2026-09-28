//! Standalone `susi-sandbox` leaf service (port from `LEAF_SERVICES`).

fn main() -> std::io::Result<()> {
    susi_leaf_services::run_standalone("susi-sandbox")
}
