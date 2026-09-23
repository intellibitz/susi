//! Apply a patch and run tests, with rollback on failure.
use anyhow::{bail, Result};
use std::path::Path;

#[derive(Debug, clap::Args)]
pub struct PatchApplyArgs {
    /// JSON file containing a PatchRequest.
    file: std::path::PathBuf,
    /// Override auto_apply (still requires workspace confinement).
    #[arg(long)]
    auto_apply: bool,
}

pub fn execute(args: PatchApplyArgs, workspace: &Path) -> Result<()> {
    let text = std::fs::read_to_string(&args.file)
        .map_err(|e| anyhow::anyhow!("cannot read patch file: {e}"))?;
    let request: susi_gawd::patch_cycle::PatchRequest = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("invalid patch request JSON: {e}"))?;
    if request.files.is_empty() {
        bail!("patch request contains no files");
    }
    let cfg = susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
    let mut req = request;
    if args.auto_apply {
        req.auto_apply = true;
    }
    let outcome = susi_gawd::patch_cycle::apply_patch_cycle(workspace, &req, &cfg.trust_level())?;
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    Ok(())
}
