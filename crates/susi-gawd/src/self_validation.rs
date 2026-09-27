// Empirical substrate self-validation: hardware profile, tensor device, and
// compiled-genome integrity, logged to the audit trail. Lives in `gawd`
// (not `daemon`, despite its origin in `SusiRuntimeAdmin`) because it has no
// daemon-lifecycle dependency and `gmcp`'s `self_validate` tool needs to
// call it without `gmcp` depending on `daemon` (which itself depends on
// `gmcp` to start the GMCP server — a real cycle this avoids).

use crate::susi_core::plane_bus::gemi::HardwareProfiler;
use crate::susi_error::EaiResult;
use crate::susi_sandbox::manager::SusiAuditLogger;
use std::path::Path;

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

    // The one check this report makes: the compiled genome is present and
    // every rule has a title and an imperative. (It used to print "N
    // Compiled Rules Verified." after only counting them.)
    let rules = crate::self_core::AlphaSelf::RULES;
    let blank = rules
        .iter()
        .filter(|r| r.title.trim().is_empty() || r.imperative.trim().is_empty())
        .count();
    if rules.is_empty() || blank > 0 {
        return Err(crate::susi_error::EaiError::governance(format!(
            "compiled genome invalid: {} rules, {blank} with a blank title or imperative",
            rules.len()
        )));
    }
    report.push_str(&format!(
        "- **Compiled Genome**: {} rules, each with a title and an imperative\n",
        rules.len()
    ));

    SusiAuditLogger::log_event(
        workspace,
        "SELF_VALIDATION",
        "Self-validation report generated (hardware profile, tensor device, compiled genome).",
    );

    Ok(report)
}
