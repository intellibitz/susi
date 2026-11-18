#[path = "linux_link.rs"]
mod linux_link;

fn main() -> std::io::Result<()> {
    linux_link::configure_for(&[
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
        "local-harvest",
        "local-distill",
        "release-qualify",
    ])
}
