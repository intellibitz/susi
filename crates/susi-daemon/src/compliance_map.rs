//! Compliance control map (Swarm OS Bullet 95)
//!
//! Each row names a control and the daemon module that implements the
//! corresponding HMAC or residency behavior. This is the engine's control
//! map, not an attestation that a deployment is certified.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    pub framework: &'static str,
    pub control_id: &'static str,
    pub module: &'static str,
}

pub fn controls() -> &'static [Control] {
    &[
        Control {
            framework: "soc2",
            control_id: "cc7.2",
            module: "audit_log",
        },
        Control {
            framework: "iso27001",
            control_id: "a.12.4",
            module: "audit_log",
        },
        Control {
            framework: "gdpr",
            control_id: "art32",
            module: "residency",
        },
        Control {
            framework: "hipaa",
            control_id: "164.312",
            module: "secret_store",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_audit_and_residency_are_mapped() {
        assert!(
            controls()
                .iter()
                .any(|control| { control.framework == "soc2" && control.module == "audit_log" })
        );
        assert!(
            controls()
                .iter()
                .any(|control| control.framework == "gdpr" && control.module == "residency")
        );
    }
}
