//! Pure-standard-library `#![forbid(unsafe_code)]` lowercase hexadecimal encoding.

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

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

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn encode_formats_lowercase_hex_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode([0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
