//! The selected Linux OpenSSL binding compiles Rust only. expando.c is
//! preprocessed for header constants, never linked. This admission is provisional:
//! release qualification must also verify ELF imports and linker-map inputs.
use std::path::Path;

pub(crate) fn supported(host: &str) -> bool {
    matches!(
        host,
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
    )
}

pub(crate) fn admit(
    host: &str,
    version: &str,
    features: &[String],
    manifest: &Path,
) -> super::Result<()> {
    if !super::elf_imports::reviewed_openssl("openssl-sys", version, host, features) {
        return Err("unreviewed OpenSSL target, binding version or features".into());
    }
    let root = manifest.parent().ok_or("OpenSSL manifest has no parent")?;
    if super::file_digest(manifest)?
        != "0398fda9f72001a6d7f9dad48cf7c04a8e84161aa8a2342fc183247f90190085"
    {
        return Err("OpenSSL binding manifest changed; requires review".into());
    }
    let expected = [
        (
            "cfgs.rs",
            "9bb209657f5558a39740c4eb954404c3164b3c45fa6421b2916e2a7f18d6e300",
        ),
        (
            "expando.c",
            "a44f747b4fe126b0afb105be40585c1bf25262487b413921310f6682e33a8a9c",
        ),
        (
            "find_normal.rs",
            "015b5d233c175a67111743c25839a1917242b575f9bbf2fa36582d060c9d1696",
        ),
        (
            "find_vendored.rs",
            "2d565046018a6855b6fbffa47dc6a28b027704ba234b1a00c1fe336b88e3745f",
        ),
        (
            "main.rs",
            "3a7f63b3c446451801ac03c34e03051b8d3ebcceea28b8fb2688922255629d27",
        ),
        (
            "run_bindgen.rs",
            "8d7ff6933dfd21910ce823662d26c14ff6b66a72b75a3feb147bed3bb2b4789e",
        ),
    ];
    for (name, digest) in expected {
        if super::file_digest(&root.join("build").join(name))? != digest {
            return Err("OpenSSL binding build source changed; requires review".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_targets_and_compiling_feature_variants_are_rejected() {
        for features in [
            vec!["vendored".into()],
            vec!["bindgen".into()],
            vec!["aws-lc".into()],
        ] {
            assert!(admit(
                "x86_64-unknown-linux-gnu",
                "0.9.117",
                &features,
                Path::new("missing")
            )
            .is_err());
        }
        assert!(admit(
            "x86_64-pc-windows-msvc",
            "0.9.117",
            &[],
            Path::new("missing")
        )
        .is_err());
        assert!(admit(
            "x86_64-unknown-linux-gnu",
            "0.9.118",
            &[],
            Path::new("missing")
        )
        .is_err());
        assert!(admit(
            "x86_64-unknown-linux-gnu",
            "0.9.117",
            &[],
            Path::new("missing")
        )
        .is_err());
    }
}
