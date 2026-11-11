//! The value formats the A2A wire carries: standard base64 for raw parts,
//! UUIDs for generated ids (version 7 for tasks, contexts and messages,
//! version 4 for push configs) and RFC 3339 UTC timestamps.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn morsel(byte: u8) -> Option<u8> {
    ALPHABET.iter().position(|&a| a == byte).map(|p| p as u8)
}

/// Standard base64 with canonical padding and zero trailing bits. A refusal
/// reads exactly as the `base64` 0.22 crate's (`Invalid symbol 33, offset
/// 3.`, `Invalid input length: 5`, `Invalid padding`, `Invalid last symbol
/// 57, offset 6.`), the text clients saw in a params error.
pub fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let input = input.as_bytes();
    let invalid_byte = |offset: usize, byte: u8| format!("Invalid symbol {byte}, offset {offset}.");
    let rem = input.len() % 4;
    if rem == 1 {
        let last = input[input.len() - 1];
        if last != b'=' && morsel(last).is_none() {
            return Err(invalid_byte(input.len() - 1, last));
        }
    }
    let quads = input.len() - rem - if rem == 0 { 4.min(input.len()) } else { 0 };
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    for (q, quad) in input[..quads].as_chunks::<4>().0.iter().enumerate() {
        let mut n = 0u32;
        for (i, &byte) in quad.iter().enumerate() {
            let m = morsel(byte).ok_or_else(|| invalid_byte(q * 4 + i, byte))?;
            n = (n << 6) | u32::from(m);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..]);
    }
    let mut morsels = [0u8; 4];
    let mut count = 0;
    let mut padding = 0;
    let mut first_padding = 0;
    let mut last_symbol = 0;
    for (i, &byte) in input[quads..].iter().enumerate() {
        if byte == b'=' {
            if i < 2 {
                return Err(invalid_byte(quads + i, byte));
            }
            if padding == 0 {
                first_padding = i;
            }
            padding += 1;
            continue;
        }
        if padding > 0 {
            return Err(invalid_byte(quads + first_padding, b'='));
        }
        last_symbol = byte;
        morsels[count] = morsel(byte).ok_or_else(|| invalid_byte(quads + i, byte))?;
        count += 1;
    }
    if !input.is_empty() && count < 2 {
        return Err(format!("Invalid input length: {}", quads + count));
    }
    if (padding + count) % 4 != 0 {
        return Err("Invalid padding".to_string());
    }
    let tail = count * 6 / 8;
    let n = morsels
        .iter()
        .enumerate()
        .fold(0u32, |n, (i, m)| n | (u32::from(*m) << (26 - 6 * i)));
    if n & (u32::MAX.checked_shr(tail as u32 * 8).unwrap_or(0)) != 0 {
        return Err(format!(
            "Invalid last symbol {last_symbol}, offset {}.",
            quads + count - 1
        ));
    }
    out.extend_from_slice(&n.to_be_bytes()[..tail]);
    Ok(out)
}

fn random_bytes() -> [u8; 16] {
    let hex = susi_config::cluster_key::random_nonce_hex();
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = hex
            .get(i * 2..i * 2 + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .unwrap_or(0);
    }
    bytes
}

fn uuid_string(mut bytes: [u8; 16], version: u8) -> String {
    bytes[6] = (bytes[6] & 0x0f) | (version << 4);
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A random (version 4) UUID.
pub fn uuid_v4() -> String {
    uuid_string(random_bytes(), 4)
}

/// A time-ordered (version 7) UUID: Unix milliseconds, then random bits.
pub fn uuid_v7() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut bytes = random_bytes();
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    uuid_string(bytes, 7)
}

/// Now as RFC 3339 in UTC with a `+00:00` offset and the second fraction
/// in 0, 3, 6 or 9 digits, whichever is exact.
pub fn rfc3339_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339(now.as_secs() as i64, now.subsec_nanos())
}

fn rfc3339(secs: i64, nanos: u32) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let fraction = if nanos == 0 {
        String::new()
    } else if nanos.is_multiple_of(1_000_000) {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos.is_multiple_of(1_000) {
        format!(".{:06}", nanos / 1_000)
    } else {
        format!(".{nanos:09}")
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{fraction}+00:00",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

#[cfg(test)]
#[path = "../tests/codec.rs"]
mod tests;
