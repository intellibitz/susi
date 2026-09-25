//! Data residency (Swarm OS Bullet 56)
//!
//! A memory write is admitted only when the cell's region equals the
//! memory's region. There is no implicit "any region" grant.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyDecision {
    Allow,
    Deny,
}

pub fn admit_write(cell_region: &str, memory_region: &str) -> ResidencyDecision {
    if !cell_region.is_empty() && cell_region == memory_region {
        ResidencyDecision::Allow
    } else {
        ResidencyDecision::Deny
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_across_regions_is_denied() {
        assert_eq!(admit_write("eu", "eu"), ResidencyDecision::Allow);
        assert_eq!(admit_write("eu", "us"), ResidencyDecision::Deny);
        assert_eq!(admit_write("", "eu"), ResidencyDecision::Deny);
    }
}
