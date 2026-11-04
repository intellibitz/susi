//! Engine hooks re-export — implementation lives in `susi-daemon`, the
//! composition-root crate that sees gawd + gemi + gmcp.

#![warn(missing_docs)]

pub use susi_daemon::SusiEngineHooks;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn susi_engine_hooks_is_zero_sized_unit_struct() {
        let hooks = SusiEngineHooks;
        let _ = &hooks;
        assert_eq!(std::mem::size_of::<SusiEngineHooks>(), 0);
    }

    #[test]
    fn susi_engine_hooks_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SusiEngineHooks>();
    }

    #[test]
    fn susi_engine_hooks_alignment_is_one() {
        assert_eq!(std::mem::align_of::<SusiEngineHooks>(), 1);
    }

    #[test]
    fn susi_engine_hooks_boxes_without_heap_allocation() {
        let boxed = Box::new(SusiEngineHooks);
        assert_eq!(std::mem::size_of_val(&*boxed), 0);
    }
}
