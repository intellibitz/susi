use std::env;
use std::fs;

pub fn push_to_hardware_if_dev_build() {
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

            if should_install {
                let _ = fs::create_dir_all(&bin_dir);
                let _ = fs::remove_file(&target);
                let live_exe = {
                    let proc_exe = std::path::PathBuf::from("/proc/self/exe");
                    if proc_exe.exists() { proc_exe } else { exe.clone() }
                };
                if fs::copy(&live_exe, &target).is_ok() {
                    // stderr, not stdout — operator commands emit
                    // machine-readable JSON on stdout (`crown verify`,
                    // `os --json`); a deploy banner there corrupts parsing.
                    eprintln!("[SUBSTRATE DEPLOYMENT] Development build natively overriding hardware daemon path.");
                }
            }
        }
    }
}
