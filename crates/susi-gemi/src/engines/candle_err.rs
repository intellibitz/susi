//! Convert Candle errors at the GEMI adapter edge (not in `susi-error`).

use crate::susi_error::EaiError;

/// Map a Candle error into [`EaiError::Inference`].
#[inline]
pub fn from_candle(err: susi_vendor_candle::candle_core::Error) -> EaiError {
    EaiError::inference(err.to_string())
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use susi_vendor_candle::candle_core::Error as CandleError;

    #[test]
    fn from_candle_maps_msg_error_into_inference_variant() {
        let err = from_candle(CandleError::Msg("tensor shape mismatch".to_string()));
        assert!(matches!(err, EaiError::Inference(..)));
        assert!(err.to_string().contains("tensor shape mismatch"));
    }

    #[test]
    fn from_candle_preserves_detailed_context_string() {
        let err = from_candle(CandleError::Msg("forward pass layer 12 failed".to_string()));
        assert!(err.to_string().contains("forward pass layer 12 failed"));
    }

    #[test]
    fn from_candle_maps_wrapped_context_error_cleanly() {
        let base = CandleError::Msg("bad dtype".to_string()).context("attention projection");
        let mapped = from_candle(base);
        let msg = mapped.to_string();
        assert!(msg.contains("bad dtype"));
        assert!(msg.contains("attention projection"));
    }

    #[test]
    fn from_candle_produces_non_empty_display_representation() {
        let mapped = from_candle(CandleError::Msg("oom".to_string()));
        assert!(!mapped.to_string().trim().is_empty());
    }

    #[test]
    fn from_candle_preserves_multi_level_chained_context() {
        let chained = CandleError::Msg("nan logit".to_string())
            .context("decoder layer 4")
            .context("greedy sample step");
        let mapped = from_candle(chained);
        let text = mapped.to_string();
        assert!(text.contains("nan logit"));
        assert!(text.contains("decoder layer 4"));
        assert!(text.contains("greedy sample step"));
    }
}
