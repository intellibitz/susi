//! Green scheduling (Swarm OS Bullet 97)
//!
//! Heavy work is admitted when renewable supply is available, or when the
//! host is not under thermal stress. Thermal stress without renewable
//! supply defers the work. `thermal_stress` is the flag
//! `runtime_admin` already publishes on `hardware.stress`.

pub fn heavy_work_allowed(thermal_stress: bool, renewable_available: bool) -> bool {
    renewable_available || !thermal_stress
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thermal_stress_defers_heavy_work_unless_renewable_supply_is_available() {
        assert!(!heavy_work_allowed(true, false));
        assert!(heavy_work_allowed(true, true));
        assert!(heavy_work_allowed(false, false));
    }
}
