//! Value formats on the A2A wire: base64 against the base64 0.22 crate's
//! STANDARD engine, UUID versions and chrono-style RFC 3339 timestamps.

use super::*;

#[test]
fn base64_round_trips_and_refuses_like_the_base64_crate() {
    for raw in [&b""[..], b"h", b"he", b"hel", b"hello", b"\xff\x00\x10"] {
        assert_eq!(base64_decode(&base64_encode(raw)).unwrap(), raw);
    }
    assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    assert_eq!(base64_encode(b"he"), "aGU=");
    // Reference: the base64 0.22.1 crate's STANDARD engine on each input.
    assert_eq!(base64_decode("abcd").unwrap(), [105, 183, 29]);
    assert_eq!(base64_decode("abc=").unwrap(), [105, 183]);
    for (input, error) in [
        ("aGV!bG8=", "Invalid symbol 33, offset 3."),
        ("aGVsb", "Invalid input length: 5"),
        ("aGVsbG8", "Invalid padding"),
        ("aGVsbG9=", "Invalid last symbol 57, offset 6."),
        ("aG=sbG8=", "Invalid symbol 61, offset 2."),
        ("a===", "Invalid symbol 61, offset 1."),
        ("aGU=a", "Invalid symbol 61, offset 3."),
        ("aGVsbG8=!", "Invalid symbol 33, offset 8."),
        ("aG==bG8=", "Invalid symbol 61, offset 2."),
        ("aGU=x===", "Invalid symbol 61, offset 3."),
        ("=", "Invalid symbol 61, offset 0."),
        ("==", "Invalid symbol 61, offset 0."),
        ("a", "Invalid input length: 1"),
        ("ab", "Invalid padding"),
        ("abc", "Invalid padding"),
        ("ab==", "Invalid last symbol 98, offset 1."),
        ("ab=c", "Invalid symbol 61, offset 2."),
        ("abcde===", "Invalid symbol 61, offset 5."),
        ("aGVsbG8=aGVsbG8=", "Invalid symbol 61, offset 7."),
        ("aGVs\nbG8=", "Invalid symbol 10, offset 4."),
        ("ab=", "Invalid padding"),
        ("a=", "Invalid symbol 61, offset 1."),
        ("abc==", "Invalid symbol 61, offset 3."),
        ("abcd=", "Invalid symbol 61, offset 4."),
    ] {
        assert_eq!(base64_decode(input).unwrap_err(), error, "{input}");
    }
}

#[test]
fn uuids_carry_their_version_and_variant() {
    for (id, version) in [(uuid_v4(), '4'), (uuid_v7(), '7')] {
        assert_eq!(id.len(), 36, "{id}");
        assert_eq!(id.as_bytes()[14], version as u8, "{id}");
        assert!(
            matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "{id}"
        );
        assert_eq!(
            id.char_indices()
                .filter(|(_, c)| *c == '-')
                .map(|(i, _)| i)
                .collect::<Vec<_>>(),
            [8, 13, 18, 23]
        );
    }
    assert_ne!(uuid_v7(), uuid_v7());
}

#[test]
fn timestamps_read_like_chrono_to_rfc3339() {
    assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00+00:00");
    assert_eq!(
        rfc3339(1_794_363_042, 120_000_000),
        "2026-11-11T02:10:42.120+00:00"
    );
    assert_eq!(
        rfc3339(951_782_400, 5_000),
        "2000-02-29T00:00:00.000005+00:00"
    );
    assert_eq!(rfc3339(-1, 7), "1969-12-31T23:59:59.000000007+00:00");
}
