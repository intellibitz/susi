use std::env;
use std::fs;
use std::path::Path;

/// How a `susi` binary was built. Recorded next to the installed binary in
/// `susi.build.json` so a later dev build can tell whether replacing it would
/// be a downgrade (debug over release, or CPU-only over an accelerated
/// build). The composition root supplies it because features and profile
/// are properties of the root package, not of this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildIdentity {
    pub release: bool,
    /// `cuda` / `metal` / `mkl`, or `None` for a CPU-only build.
    pub accelerator: Option<&'static str>,
}

/// Sidecar marker describing the installed binary; written by every
/// installer (`cargo xb` and this dev self-install).
pub const BUILD_MARKER: &str = "susi.build.json";

/// Operator override: install the dev build even when it downgrades.
pub const ALLOW_DOWNGRADE_ENV: &str = "SUSI_ALLOW_INSTALL_DOWNGRADE";

impl BuildIdentity {
    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "profile": if self.release { "release" } else { "debug" },
            "accelerator": self.accelerator,
        })
    }
}

/// Why installing `candidate` over `installed` would be a downgrade, if it
/// would. `installed` is the raw marker JSON; an unreadable marker yields
/// `None` so a corrupt sidecar never blocks development.
fn downgrade_reason(installed: &serde_json::Value, candidate: BuildIdentity) -> Option<String> {
    let installed_release = installed.get("profile").and_then(|p| p.as_str()) == Some("release");
    let installed_accel = installed.get("accelerator").and_then(|a| a.as_str());
    if installed_release && !candidate.release {
        return Some("a debug build would replace the installed release build".to_string());
    }
    match installed_accel {
        Some(accel) if candidate.accelerator != Some(accel) => Some(format!(
            "a build without `{accel}` would replace the installed `{accel}` build"
        )),
        Some(_) | None => None,
    }
}

fn read_marker(bin_dir: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(bin_dir.join(BUILD_MARKER)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn push_to_hardware_if_dev_build(identity: BuildIdentity) {
    if let Ok(exe) = env::current_exe() {
        let exe_str = exe.to_string_lossy();
        // Gate on the reported path, but copy the *live inode*: an in-place
        // replacement leaves `current_exe` pointing at a `… (deleted)` path
        // whose copy would silently fail. `/proc/self/exe` always resolves
        // the running image (Linux); elsewhere the reported path is used.
        if exe_str.contains("target/debug") || exe_str.contains("target/release") {
            let home = crate::susi_paths::SusiDirs::home_dir();
            let bin_dir = home.join(".susi").join("bin");
            let target = bin_dir.join(if cfg!(windows) { "susi.exe" } else { "susi" });

            let mut should_install = true;
            if let Ok(meta_self) = fs::metadata(&exe) {
                if let Ok(meta_target) = fs::metadata(&target) {
                    if let (Ok(m1), Ok(m2)) = (meta_self.modified(), meta_target.modified()) {
                        if m1 <= m2 {
                            should_install = false;
                        }
                    }
                }
            }

            if should_install && target.exists() && env::var_os(ALLOW_DOWNGRADE_ENV).is_none() {
                if let Some(reason) =
                    read_marker(&bin_dir).and_then(|marker| downgrade_reason(&marker, identity))
                {
                    eprintln!(
                        "[SUBSTRATE DEPLOYMENT] Skipped: {reason}. Set {ALLOW_DOWNGRADE_ENV}=1 to install anyway."
                    );
                    should_install = false;
                }
            }

            if should_install {
                let _ = fs::create_dir_all(&bin_dir);
                let _ = fs::remove_file(&target);
                let live_exe = {
                    let proc_exe = std::path::PathBuf::from("/proc/self/exe");
                    if proc_exe.exists() {
                        proc_exe
                    } else {
                        exe.clone()
                    }
                };
                if fs::copy(&live_exe, &target).is_ok() {
                    let _ = fs::write(bin_dir.join(BUILD_MARKER), identity.to_json().to_string());
                    // stderr, not stdout — operator commands emit
                    // machine-readable JSON on stdout (`crown verify`,
                    // `os --json`); a deploy banner there corrupts parsing.
                    eprintln!("[SUBSTRATE DEPLOYMENT] Development build natively overriding hardware daemon path.");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBUG_CPU: BuildIdentity = BuildIdentity {
        release: false,
        accelerator: None,
    };
    const RELEASE_CPU: BuildIdentity = BuildIdentity {
        release: true,
        accelerator: None,
    };
    const RELEASE_CUDA: BuildIdentity = BuildIdentity {
        release: true,
        accelerator: Some("cuda"),
    };

    #[test]
    fn debug_never_replaces_release() {
        let installed = RELEASE_CPU.to_json();
        assert!(downgrade_reason(&installed, DEBUG_CPU).is_some_and(|r| r.contains("debug build")));
        assert!(downgrade_reason(&installed, RELEASE_CPU).is_none());
    }

    #[test]
    fn accelerated_install_requires_the_same_accelerator() {
        let installed = RELEASE_CUDA.to_json();
        assert!(downgrade_reason(&installed, RELEASE_CPU).is_some_and(|r| r.contains("cuda")));
        assert!(downgrade_reason(&installed, RELEASE_CUDA).is_none());
        let metal = BuildIdentity {
            release: true,
            accelerator: Some("metal"),
        };
        assert!(downgrade_reason(&installed, metal).is_some());
    }

    #[test]
    fn upgrades_and_unreadable_markers_are_allowed() {
        assert!(downgrade_reason(&DEBUG_CPU.to_json(), RELEASE_CUDA).is_none());
        assert!(downgrade_reason(&serde_json::json!("garbage"), DEBUG_CPU).is_none());
        assert!(downgrade_reason(&serde_json::json!({}), DEBUG_CPU).is_none());
    }
}
