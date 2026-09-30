//! NPU and vendor-accelerator detection (T-CLAUDE-22) — extends
//! [`crate::local_ecosystem::accelerators`] beyond cuda/rocm/metal/
//! vulkan/oneapi with Apple ANE, Intel NPU, Qualcomm NPU and AMD XDNA NPUs
//! where the host exposes evidence, plus which engine backends can use them.

use crate::local_ecosystem::Probe;

/// A detected neural/accelerator device.
#[derive(Debug, Clone, PartialEq)]
pub struct Npu {
    /// Backend id reported through `accelerators()`.
    pub backend: &'static str,
    /// Human evidence string (device node or tool path).
    pub evidence: String,
}

/// Engines that can drive each NPU backend (def ids from
/// `local_ecosystem::DEFS` and provider families).
pub fn engines_for(backend: &str) -> &'static [&'static str] {
    match backend {
        "ane" => &["coreml", "mlx"],
        "intel-npu" => &["openvino", "onnxruntime-openvino"],
        "qualcomm-npu" => &["qnn", "onnxruntime-qnn"],
        "amd-npu" => &["ryzen-ai", "openvino", "onnxruntime-vitisai"],
        _ => &[],
    }
}

/// Detect NPU-class accelerators not covered by the GPU backends.
/// Evidence used (in order):
/// - Apple: `is_macos_arm()` — the ANE is always present on Apple silicon.
/// - Intel: `/dev/accel/accel*` nodes with no `xrt-smi` (XDNA shares the
///   accel subsystem; AMD takes precedence when its tool is found).
/// - AMD XDNA: `xrt-smi` on PATH or `/dev/accel/accel*` + xrt libs.
/// - Qualcomm: `qnn-net-run`/`qnn-context-binary-generator` on PATH.
pub fn npus(p: &dyn Probe) -> Vec<Npu> {
    let mut out = Vec::new();
    if p.is_macos_arm() {
        out.push(Npu {
            backend: "ane",
            evidence: "Apple Neural Engine (Apple silicon)".into(),
        });
    }
    let accel = std::path::Path::new("/dev/accel/accel0");
    if p.which("xrt-smi").is_some() {
        out.push(Npu {
            backend: "amd-npu",
            evidence: "xrt-smi".into(),
        });
    } else if p.exists(accel) {
        out.push(Npu {
            backend: "intel-npu",
            evidence: "/dev/accel/accel0".into(),
        });
    }
    if let Some(path) = p
        .which("qnn-net-run")
        .or_else(|| p.which("qnn-context-binary-generator"))
    {
        out.push(Npu {
            backend: "qualcomm-npu",
            evidence: path.display().to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct FakeProbe {
        tools: Vec<&'static str>,
        files: Vec<&'static str>,
        macos_arm: bool,
    }

    impl Probe for FakeProbe {
        fn which(&self, name: &str) -> Option<PathBuf> {
            self.tools
                .iter()
                .find(|t| **t == name)
                .map(|t| PathBuf::from(format!("/usr/bin/{t}")))
        }
        fn home(&self) -> Option<PathBuf> {
            None
        }
        fn exists(&self, path: &Path) -> bool {
            self.files.iter().any(|f| Path::new(f) == path)
        }
        fn is_macos_arm(&self) -> bool {
            self.macos_arm
        }
        fn http_ok(&self, _port: u16, _path: &str) -> bool {
            false
        }
        fn version(&self, _program: &Path) -> Option<String> {
            None
        }
    }

    fn probe(tools: &[&'static str], files: &[&'static str], macos_arm: bool) -> FakeProbe {
        FakeProbe {
            tools: tools.to_vec(),
            files: files.to_vec(),
            macos_arm,
        }
    }

    #[test]
    fn npu_detection_apple_silicon_reports_ane() {
        let p = probe(&[], &[], true);
        let n = npus(&p);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].backend, "ane");
        assert!(engines_for("ane").contains(&"mlx"));
    }

    #[test]
    fn npu_detection_intel_from_accel_node() {
        let p = probe(&[], &["/dev/accel/accel0"], false);
        let n = npus(&p);
        assert_eq!(
            n.iter().map(|x| x.backend).collect::<Vec<_>>(),
            ["intel-npu"]
        );
    }

    #[test]
    fn npu_detection_xrt_smi_wins_over_plain_accel() {
        // /dev/accel exists on XDNA too — the xrt tool disambiguates to AMD
        let p = probe(&["xrt-smi"], &["/dev/accel/accel0"], false);
        let n = npus(&p);
        assert!(n.iter().all(|x| x.backend != "intel-npu"));
        assert!(n.iter().any(|x| x.backend == "amd-npu"));
    }

    #[test]
    fn npu_detection_qualcomm_from_qnn_tool() {
        let p = probe(&["qnn-net-run"], &[], false);
        let n = npus(&p);
        assert_eq!(n[0].backend, "qualcomm-npu");
        assert!(engines_for("qualcomm-npu").contains(&"qnn"));
    }

    #[test]
    fn npu_detection_none_on_plain_host() {
        let p = probe(&[], &[], false);
        assert!(npus(&p).is_empty());
    }

    #[test]
    fn npu_detection_accel_integrates_with_accelerators() {
        // accelerators() should pick the NPU entries up through npus()
        let p = probe(&["qnn-net-run"], &["/dev/accel/accel0"], false);
        let acc = crate::local_ecosystem::accelerators(&p);
        let backends: Vec<&str> = acc.iter().map(|a| a.backend).collect();
        assert!(backends.contains(&"intel-npu"), "{backends:?}");
        assert!(backends.contains(&"qualcomm-npu"), "{backends:?}");
    }
}
