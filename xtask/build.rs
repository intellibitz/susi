fn main() -> std::io::Result<()> {
    susi_build_os_ucrt::configure_for(&[
        "xtask",
        "local-workflow",
        "push-policy",
        "local-release",
        "ci-release-only",
        "susi-dev",
        "local-snapshot",
    ])
}
