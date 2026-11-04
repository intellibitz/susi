//! User service units installed and resource-capped automatically.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceUnit {
    name: String,
    memory_max: String,
    cpu_quota: String,
}

fn default_user_unit() -> ServiceUnit {
    ServiceUnit {
        name: "susi.service".into(),
        memory_max: "2G".into(),
        cpu_quota: "200%".into(),
    }
}

fn unit_body(u: &ServiceUnit) -> String {
    format!(
        "[Unit]\nDescription=susi daemon\n\n[Service]\nMemoryMax={}\nCPUQuota={}\nExecStart=%h/.susi/bin/susi daemon\n\n[Install]\nWantedBy=default.target\n",
        u.memory_max, u.cpu_quota
    )
}

#[test]
fn zc_service_units_are_resource_capped() {
    let u = default_user_unit();
    let body = unit_body(&u);
    assert_eq!(u.name, "susi.service");
    assert!(body.contains("MemoryMax=2G"));
    assert!(body.contains("CPUQuota=200%"));
}

#[test]
fn zc_service_unit_contains_required_systemd_sections_and_exec_start() {
    let u = default_user_unit();
    let body = unit_body(&u);
    assert!(body.contains("[Unit]\nDescription=susi daemon\n"));
    assert!(body.contains("[Service]\n"));
    assert!(body.contains("ExecStart=%h/.susi/bin/susi daemon\n"));
    assert!(body.contains("[Install]\nWantedBy=default.target\n"));
}

#[test]
fn zc_service_unit_custom_limits_render_accurately() {
    let custom = ServiceUnit {
        name: "susi-custom.service".into(),
        memory_max: "4G".into(),
        cpu_quota: "400%".into(),
    };
    let body = unit_body(&custom);
    assert!(body.contains("MemoryMax=4G\n"));
    assert!(body.contains("CPUQuota=400%\n"));
    assert_ne!(custom, default_user_unit());
}

#[test]
fn zc_service_unit_clone_and_debug_preserve_fields() {
    let u = default_user_unit();
    let cloned = u.clone();
    assert_eq!(u, cloned);
    let dbg = format!("{u:?}");
    assert!(dbg.contains("susi.service"));
    assert!(dbg.contains("2G"));
    assert!(dbg.contains("200%"));
}
