//! All credential-free native launch paths opt out of persistent environment
//! fallback explicitly, after scrubbing inherited SUSI_* process variables.
#![allow(clippy::expect_used)] // Source-contract assertions describe missing launcher wiring.

#[test]
fn qualification_and_test_launchers_disable_registry_fallback() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin");
    for (file, count) in [
        ("local-workflow.rs", 3),
        ("local-release.rs", 2),
        ("local-channel.rs", 2),
        ("startup-gate.rs", 1),
    ] {
        let source = std::fs::read_to_string(root.join(file)).expect("launcher source");
        let production = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("production source");
        assert_eq!(
            production
                .matches(".env(\"SUSI_NO_REGISTRY_ENV\", \"1\")")
                .count(),
            count,
            "{file}"
        );
    }
}
