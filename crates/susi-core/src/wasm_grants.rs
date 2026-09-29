//! Attenuate grants for generated Wasm capabilities (VC-201-075).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantSet {
    pub caps: BTreeSet<String>,
}

impl GrantSet {
    #[must_use]
    pub fn host_baseline() -> Self {
        Self {
            caps: ["fs.read", "net.loopback", "clock"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        }
    }

    /// Generated Wasm never receives more than the attenuated subset.
    #[must_use]
    pub fn attenuate_for_wasm(&self) -> Self {
        let allow = BTreeSet::from(["fs.read".to_string(), "clock".to_string()]);
        Self {
            caps: self.caps.intersection(&allow).cloned().collect(),
        }
    }

    #[must_use]
    pub fn allows(&self, cap: &str) -> bool {
        self.caps.contains(cap)
    }
}
