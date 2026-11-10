//! Vendor facade for `getrandom`, unified on the workspace's single `0.2` line
//! so OS entropy is compiled and linked once across Susi Core and platform TLS/crypto.
#![forbid(unsafe_code)]

pub use getrandom::{getrandom, Error};

/// Fill `dest` with cryptographic OS random bytes.
pub fn fill(dest: &mut [u8]) -> Result<(), Error> {
    getrandom::getrandom(dest)
}

#[cfg(test)]
// Unit tests assert directly on OS CSPRNG buffer fills via unwrap/expect.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn fill_populates_entropy_and_empty_slice_succeeds() {
        let mut empty = [];
        fill(&mut empty).unwrap();

        let mut a = [0_u8; 32];
        let mut b = [0_u8; 32];
        fill(&mut a).unwrap();
        fill(&mut b).unwrap();
        assert_ne!(a, [0_u8; 32]);
        assert_ne!(a, b);
    }
}
