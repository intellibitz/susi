use std::env;
use std::fs;

pub fn push_to_hardware_if_dev_build() {
    if let Ok(exe) = env::current_exe() {
        let exe_str = exe.to_string_lossy();
        if exe_str.contains("target/debug") || exe_str.contains("target/release") {
            let home = susi_paths::SusiDirs::home_dir();
            let bin_dir = home.join(".susi").join("bin");
            let target = bin_dir.join(if cfg!(windows) {
                "susi-engine.exe"
            } else {
                "susi-engine"
            });

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
                if fs::copy(&exe, &target).is_ok() {
                    let susi_name = if cfg!(windows) { "susi.exe" } else { "susi" };
                    let susi_lnk = bin_dir.join(susi_name);
                    let _ = fs::remove_file(&susi_lnk);
                    #[cfg(windows)]
                    {
                        let _ = fs::copy(&target, &susi_lnk);
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::symlink;
                        let _ = symlink("susi-engine", &susi_lnk);
                    }
                    println!("[SUBSTRATE DEPLOYMENT] Development build natively overriding hardware daemon path.");
                }
            }
        }
    }
}
