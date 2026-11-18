//! Explicit native sysroot contracts. Unknown targets fail before building.
//! Core ships for Linux only (Mandate 84), so only Linux hosts are admitted.
pub fn supported(target: &str) -> bool {
    matches!(
        target,
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_native_linux_targets_are_admitted() {
        for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
            assert!(super::supported(target));
        }
        assert!(!super::supported("x86_64-pc-windows-msvc"));
        assert!(!super::supported("aarch64-pc-windows-msvc"));
        assert!(!super::supported("x86_64-unknown-linux-musl"));
        assert!(!super::supported("unknown"));
    }
}
