fn main() -> std::io::Result<()> {
    susi_build_os_ucrt::configure_for(&[
        "xtask",
        "release-lanes",
        "local-workflow",
        "push-policy",
        "local-release",
        "ci-release-only",
        "check-gmcp-roadmap",
        "susi-dev",
        "local-snapshot",
        "distill",
        "local-distill",
        "release-qualify",
    ])
}
