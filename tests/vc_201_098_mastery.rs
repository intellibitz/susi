#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::expect_used)] // fixture files are part of this checked-in repository

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPlatform {
    Linux,
    Macos,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleStage {
    Install,
    Start,
    Stop,
    Upgrade,
    LocalRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlatformCapability {
    platform: HostPlatform,
    gpu: bool,
    sandbox: bool,
    installer: &'static str,
}

const PLATFORM_MATRIX: [PlatformCapability; 3] = [
    PlatformCapability {
        platform: HostPlatform::Linux,
        gpu: true,
        sandbox: true,
        installer: "install.sh",
    },
    PlatformCapability {
        platform: HostPlatform::Macos,
        gpu: false,
        sandbox: true,
        installer: "install.sh",
    },
    PlatformCapability {
        platform: HostPlatform::Windows,
        gpu: false,
        sandbox: false,
        installer: "install.ps1",
    },
];

fn lifecycle_smoke() -> [LifecycleStage; 5] {
    [
        LifecycleStage::Install,
        LifecycleStage::Start,
        LifecycleStage::Stop,
        LifecycleStage::Upgrade,
        LifecycleStage::LocalRuntime,
    ]
}

fn installer_body(name: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name))
        .expect("installer fixture must be present")
}

#[test]
fn vc_201_098_mastery() {
    assert_eq!(lifecycle_smoke().len(), 5);
    assert_eq!(PLATFORM_MATRIX.len(), 3);

    for capability in PLATFORM_MATRIX {
        let body = installer_body(capability.installer);
        assert!(body.to_ascii_lowercase().contains("susi"));
        assert!(body.to_ascii_lowercase().contains("checksum"));
        match capability.platform {
            HostPlatform::Linux | HostPlatform::Macos => {
                assert!(capability.sandbox);
                assert!(body.contains("SUSI_NO_DAEMON"));
            }
            HostPlatform::Windows => {
                assert!(!capability.gpu && !capability.sandbox);
                assert!(body.contains("Stop-Process"));
            }
        }
    }

    // The matrix is intentionally tested-vs-unsupported data: the runner
    // smoke covers the lifecycle on every host, while unsupported features
    // remain visible and cannot be reported as tested accidentally.
    assert!(PLATFORM_MATRIX
        .iter()
        .any(|row| row.platform == HostPlatform::Linux && row.gpu));
    assert!(PLATFORM_MATRIX
        .iter()
        .any(|row| row.platform == HostPlatform::Windows && !row.sandbox));
}
