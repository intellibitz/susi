//! Reference architectures (Swarm OS Bullet 94)
//!
//! The cell set each deployment tier is expected to run. Names match
//! crates and daemon modules that already exist.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Startup,
    MidMarket,
    Enterprise,
}

pub fn cells_for(tier: Tier) -> &'static [&'static str] {
    match tier {
        Tier::Startup => &["susi-gemi", "susi-gmcp"],
        Tier::MidMarket => &["susi-gemi", "susi-gmcp", "susi-gawd", "susi-sandbox"],
        Tier::Enterprise => &[
            "susi-gemi",
            "susi-gmcp",
            "susi-gawd",
            "susi-sandbox",
            "security_scan",
            "residency",
            "approval",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enterprise_includes_the_governance_modules_startup_does_not() {
        assert!(!cells_for(Tier::Startup).contains(&"approval"));
        assert!(cells_for(Tier::Enterprise).contains(&"residency"));
        assert!(cells_for(Tier::MidMarket).contains(&"susi-gawd"));
    }
}
