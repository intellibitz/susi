//! Bare `susi` status + next-step helper (zero-config UX).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BareStatus {
    pub ready: bool,
    pub next_step: String,
}

#[must_use]
pub fn bare_susi_status(daemon_up: bool, has_model: bool, keys_ok: bool) -> BareStatus {
    if !daemon_up {
        return BareStatus {
            ready: false,
            next_step: "start the daemon (susi daemon start)".into(),
        };
    }
    if !keys_ok {
        return BareStatus {
            ready: false,
            next_step: "register a cloud key when you need one (prompted on first use)".into(),
        };
    }
    if !has_model {
        return BareStatus {
            ready: false,
            next_step: "pull or select a local model (susi ecosystem scan)".into(),
        };
    }
    BareStatus {
        ready: true,
        next_step: "run a mission (susi \"your goal\")".into(),
    }
}

#[must_use]
pub fn render_bare(status: &BareStatus) -> String {
    format!(
        "susi status: {}\nnext: {}\n",
        if status.ready { "ready" } else { "not ready" },
        status.next_step
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_susi_status_prioritizes_daemon_then_keys_then_model_then_ready() {
        let daemon_down = bare_susi_status(false, false, false);
        assert!(!daemon_down.ready);
        assert_eq!(
            daemon_down.next_step,
            "start the daemon (susi daemon start)"
        );
        assert_eq!(bare_susi_status(false, true, true), daemon_down);

        let keys_missing = bare_susi_status(true, false, false);
        assert!(!keys_missing.ready);
        assert_eq!(
            keys_missing.next_step,
            "register a cloud key when you need one (prompted on first use)"
        );
        assert_eq!(bare_susi_status(true, true, false), keys_missing);

        let model_missing = bare_susi_status(true, false, true);
        assert!(!model_missing.ready);
        assert_eq!(
            model_missing.next_step,
            "pull or select a local model (susi ecosystem scan)"
        );

        let all_ready = bare_susi_status(true, true, true);
        assert!(all_ready.ready);
        assert_eq!(all_ready.next_step, "run a mission (susi \"your goal\")");
    }

    #[test]
    fn render_bare_and_serde_roundtrip_preserve_status_and_next_step() {
        let not_ready = bare_susi_status(false, true, true);
        assert_eq!(
            render_bare(&not_ready),
            "susi status: not ready\nnext: start the daemon (susi daemon start)\n"
        );

        let ready = bare_susi_status(true, true, true);
        assert_eq!(
            render_bare(&ready),
            "susi status: ready\nnext: run a mission (susi \"your goal\")\n"
        );

        let json = serde_json::to_string(&ready).expect("serialize BareStatus");
        let decoded: BareStatus = serde_json::from_str(&json).expect("deserialize BareStatus");
        assert_eq!(decoded, ready);
    }

    #[test]
    fn bare_susi_status_exhaustive_truth_table_and_render_invariants() {
        for daemon_up in [false, true] {
            for has_model in [false, true] {
                for keys_ok in [false, true] {
                    let status = bare_susi_status(daemon_up, has_model, keys_ok);
                    assert_eq!(status.ready, daemon_up && has_model && keys_ok);
                    assert!(!status.next_step.is_empty());

                    let rendered = render_bare(&status);
                    assert!(rendered.ends_with('\n'));
                    if status.ready {
                        assert!(rendered.starts_with("susi status: ready\n"));
                    } else {
                        assert!(rendered.starts_with("susi status: not ready\n"));
                    }
                }
            }
        }
    }

    #[test]
    fn bare_status_clone_equality_and_render_bare_exact_two_line_format() {
        let states = [
            bare_susi_status(true, true, true),
            bare_susi_status(false, true, true),
            bare_susi_status(true, true, false),
            bare_susi_status(true, false, true),
        ];

        for status in &states {
            let cloned = status.clone();
            assert_eq!(&cloned, status);

            let rendered = render_bare(status);
            let lines: Vec<&str> = rendered.lines().collect();
            assert_eq!(lines.len(), 2);
            let expected_status = if status.ready { "ready" } else { "not ready" };
            assert_eq!(lines[0], format!("susi status: {expected_status}"));
            assert_eq!(lines[1], format!("next: {}", status.next_step));
        }

        assert_ne!(states[0], states[1]);
        assert_ne!(states[1], states[2]);
        assert_ne!(states[2], states[3]);
    }

    #[test]
    fn bare_status_debug_output_includes_ready_and_next_step_fields() {
        let status = bare_susi_status(true, true, true);
        let dbg = format!("{status:?}");
        assert!(dbg.contains("ready: true"));
        assert!(dbg.contains("next_step"));
    }
}
