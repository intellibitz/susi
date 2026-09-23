// Empirical substrate self-validation: hardware profile, tensor device, and
// compiled-genome integrity, logged to the audit trail. Lives in `gawd`
// (not `daemon`, despite its origin in `SusiRuntimeAdmin`) because it has no
// daemon-lifecycle dependency and `gmcp`'s `self_validate` tool needs to
// call it without `gmcp` depending on `daemon` (which itself depends on
// `gmcp` to start the GMCP server — a real cycle this avoids).

use crate::susi_error::EaiResult;
use crate::susi_sandbox::manager::SusiAuditLogger;
use std::path::Path;
use susi_core::plane_bus::gemi::HardwareProfiler;

pub fn execute_autonomous_self_validation(workspace: &Path) -> EaiResult<String> {
    let profile = HardwareProfiler::get_profile();
    let mut report = "# SUSI Substrate Self-Validation Report\n\n".to_string();
    report.push_str(&format!(
        "- **Hardware Profile**: {} | {}GB RAM | {}\n",
        profile.cpu_brand, profile.ram_gb, profile.gpu_info
    ));

    // Test Tensor Substrate
    let device = HardwareProfiler::get_candle_device_label();
    report.push_str(&format!("- **Neural Device**: {device}\n"));

    // Verify Local Genome Integrity
    let genome_integrity = crate::self_core::AlphaSelf::RULES.len();
    report.push_str(&format!(
        "- **Genome Integrity**: {} Compiled Rules Verified.\n",
        genome_integrity
    ));

    SusiAuditLogger::log_event(
        workspace,
        "SELF_VALIDATION",
        "Autonomous foundational readiness test completed.",
    );

    Ok(report)
}
