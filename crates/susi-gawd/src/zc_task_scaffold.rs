//! `susi tasks add` infers the acceptance test skeleton.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptSkeleton {
    pub cmd: Vec<String>,
}

/// Infer an acceptance command from a title/filter hint.
#[must_use]
pub fn scaffold_accept(crate_name: &str, filter: &str) -> AcceptSkeleton {
    AcceptSkeleton {
        cmd: vec![
            "cargo".into(),
            "test".into(),
            "-p".into(),
            crate_name.into(),
            filter.into(),
        ],
    }
}

#[cfg(test)]
mod zc_task_scaffold_tests {
    use super::*;

    #[test]
    fn zc_task_scaffold_infers_cargo_test() {
        let s = scaffold_accept("susi-gemi", "zc_example");
        assert_eq!(s.cmd[0], "cargo");
        assert_eq!(s.cmd[3], "susi-gemi");
        assert_eq!(s.cmd[4], "zc_example");
    }
}
