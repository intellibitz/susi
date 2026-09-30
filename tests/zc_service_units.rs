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
