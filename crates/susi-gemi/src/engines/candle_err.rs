//! Convert Candle errors at the GEMI adapter edge (not in `susi-error`).

use crate::susi_error::EaiError;

/// Map a Candle error into [`EaiError::Inference`].
#[inline]
pub fn from_candle(err: candle_core::Error) -> EaiError {
    EaiError::inference(err.to_string())
}
