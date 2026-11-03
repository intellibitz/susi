//! Explicit native sysroot contracts. Unknown targets fail before building.
//! `aarch64-pc-windows-msvc` is admitted so a native ARM64 Windows host can
//! build and inventory its own private sysroot; admission is not qualification.
pub fn supported(target: &str) -> bool {
    matches!(
        target,
        "x86_64-pc-windows-msvc"
            | "aarch64-pc-windows-msvc"
            | "x86_64-unknown-linux-gnu"
            | "aarch64-unknown-linux-gnu"
    )
}

pub fn executable(target: &str, name: &str) -> String {
    if target.ends_with("-windows-msvc") {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_targets_do_not_select_foreign_executables() {
        for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
            assert!(super::supported(target));
            assert_eq!(super::executable(target, "rustc"), "rustc");
        }
        for target in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
            assert!(super::supported(target));
            assert_eq!(super::executable(target, "rustc"), "rustc.exe");
        }
        assert!(!super::supported("i686-pc-windows-msvc"));
        assert!(!super::supported("aarch64-pc-windows-gnullvm"));
        assert!(!super::supported("x86_64-unknown-linux-musl"));
        assert!(!super::supported("unknown"));
    }
}
