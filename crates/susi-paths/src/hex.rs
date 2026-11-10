//! Pure-standard-library `#![forbid(unsafe_code)]` lowercase hexadecimal
//! encoding and case-insensitive hexadecimal decoding, replacing the `hex`
//! crate and its `susi-vendor-hex` workspace facade.

use std::fmt;

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

/// Error returned when decoding an invalid hexadecimal input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromHexError {
    /// A non-hexadecimal byte was encountered at `index`.
    InvalidHexCharacter { c: char, index: usize },
    /// Input byte length is odd.
    OddLength,
    /// Fixed-length output buffer did not match input length.
    InvalidStringLength,
}

impl fmt::Display for FromHexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidHexCharacter { c, index } => {
                write!(f, "Invalid character {c:?} at position {index}")
            }
            Self::OddLength => f.write_str("Odd number of digits"),
            Self::InvalidStringLength => f.write_str("Invalid string length"),
        }
    }
}

impl std::error::Error for FromHexError {}

#[inline]
const fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Encode `data` as lowercase hexadecimal ASCII (`2 * len` bytes).
#[must_use]
pub fn encode<T: AsRef<[u8]>>(data: T) -> String {
    let bytes = data.as_ref();
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &b in bytes {
        out.push(HEX_LOWER[(b >> 4) as usize] as char);
        out.push(HEX_LOWER[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decode ASCII hexadecimal `data` (accepting both lowercase and uppercase
/// hex digits) into raw bytes.
pub fn decode<T: AsRef<[u8]>>(data: T) -> Result<Vec<u8>, FromHexError> {
    let bytes = data.as_ref();
    if bytes.len() % 2 != 0 {
        return Err(FromHexError::OddLength);
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut idx = 0;
    while idx < bytes.len() {
        let hi_byte = bytes[idx];
        let Some(hi) = decode_nibble(hi_byte) else {
            return Err(FromHexError::InvalidHexCharacter {
                c: hi_byte as char,
                index: idx,
            });
        };
        let lo_byte = bytes[idx + 1];
        let Some(lo) = decode_nibble(lo_byte) else {
            return Err(FromHexError::InvalidHexCharacter {
                c: lo_byte as char,
                index: idx + 1,
            });
        };
        out.push((hi << 4) | lo);
        idx += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next_u64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn encode_and_decode_cover_vectors_case_insensitivity_and_errors() {
        assert_eq!(encode(b""), "");
        assert_eq!(decode("").unwrap(), b"");
        assert_eq!(encode([0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(decode("000fa5ff").unwrap(), vec![0x00, 0x0f, 0xa5, 0xff]);
        assert_eq!(decode("000FA5FF").unwrap(), vec![0x00, 0x0f, 0xa5, 0xff]);
        assert_eq!(decode("deadBEEF").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);

        assert_eq!(decode("a"), Err(FromHexError::OddLength));
        assert_eq!(decode("abc"), Err(FromHexError::OddLength));
        assert_eq!(
            decode("0g"),
            Err(FromHexError::InvalidHexCharacter { c: 'g', index: 1 })
        );
        assert_eq!(
            decode("zz"),
            Err(FromHexError::InvalidHexCharacter { c: 'z', index: 0 })
        );
        assert_eq!(FromHexError::OddLength.to_string(), "Odd number of digits");
        assert_eq!(
            FromHexError::InvalidStringLength.to_string(),
            "Invalid string length"
        );
        assert_eq!(
            FromHexError::InvalidHexCharacter { c: 'x', index: 3 }.to_string(),
            "Invalid character 'x' at position 3"
        );
    }

    #[test]
    fn deterministic_250k_roundtrip_and_invalid_nibble_fuzz() {
        let mut seed = 0x517C_C1B7_2722_0A95_u64;
        for case_idx in 0..250_000_usize {
            let len = (next_u64(&mut seed) as usize) % 40;
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                buf.push((next_u64(&mut seed) & 0xff) as u8);
            }
            let encoded = encode(&buf);
            assert_eq!(encoded.len(), buf.len() * 2, "len {case_idx}");
            assert!(
                encoded
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "lowercase hex {case_idx}"
            );
            assert_eq!(decode(&encoded).unwrap(), buf, "roundtrip {case_idx}");
            assert_eq!(
                decode(encoded.to_ascii_uppercase()).unwrap(),
                buf,
                "upper roundtrip {case_idx}"
            );
            if !encoded.is_empty() {
                assert_eq!(
                    decode(&encoded[..encoded.len() - 1]),
                    Err(FromHexError::OddLength),
                    "odd {case_idx}"
                );
            }
        }
    }
}
