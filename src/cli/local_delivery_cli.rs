//! Installed Susi owns the native local delivery entry point.
// Reuse the same guarded launcher as the standalone first-build bootstrap.
#[allow(dead_code)]
#[path = "../../xtask/bootstrap/susi-dev.rs"]
mod bootstrap;

pub(crate) fn execute(args: &[String]) -> std::process::ExitCode {
    match bootstrap::run(args) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("Susi local delivery failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
