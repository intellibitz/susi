//! Model ladder edges: tiny and huge hosts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderPick {
    pub model: String,
}

#[must_use]
pub fn ladder_for_ram_gb(ram_gb: usize) -> LadderPick {
    if ram_gb < 8 {
        LadderPick {
            model: "tiny-1b".into(),
        }
    } else if ram_gb >= 64 {
        LadderPick {
            model: "huge-70b".into(),
        }
    } else {
        LadderPick {
            model: "mid-7b".into(),
        }
    }
}

#[cfg(test)]
mod zc_ladder_edges_tests {
    use super::*;

    #[test]
    fn zc_ladder_edges_tiny_and_huge() {
        assert_eq!(ladder_for_ram_gb(4).model, "tiny-1b");
        assert_eq!(ladder_for_ram_gb(128).model, "huge-70b");
        assert_eq!(ladder_for_ram_gb(16).model, "mid-7b");
    }
}
