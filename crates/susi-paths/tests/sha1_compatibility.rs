//! Compatibility vectors for the SHA-1 core shared by existing Git and STUN paths.
use susi_paths::{hex, sha1::digest_owned};

#[test]
fn rfc3174_vectors_include_multiblock_and_million_byte_messages() {
    for (bytes, expected) in [
        (b"".to_vec(), "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
        (b"abc".to_vec(), "a9993e364706816aba3e25717850c26c9cd0d89d"),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".to_vec(),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
        ),
        (
            vec![b'a'; 1_000_000],
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f",
        ),
    ] {
        assert_eq!(hex::encode(digest_owned(bytes)), expected);
    }
}

#[test]
fn padding_boundaries_match_independent_reference_digests() {
    // Fixed values independently calculated by Python hashlib.sha1, not this core.
    for (len, expected) in [
        (0, "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
        (1, "5ba93c9db0cff93f52b521d7420e43f6eda2784f"),
        (55, "8ae2d46729cfe68ff927af5eec9c7d1b66d65ac2"),
        (56, "636e2ec698dac903498e648bd2f3af641d3c88cb"),
        (63, "6d942da0c4392b123528f2905c713a3ce28364bd"),
        (64, "c6138d514ffa2135bfce0ed0b8fac65669917ec7"),
        (65, "69bd728ad6e13cd76ff19751fde427b00e395746"),
        (119, "41c89d06001bab4ab78736b44efe7ce18ce6ae08"),
        (120, "d3dbd653bd8597b7475321b60a36891278e6a04a"),
        (127, "89d7312a903f65cd2b3e34a975e55dbea9033353"),
        (128, "e6434bc401f98603d7eda504790c98c67385d535"),
        (129, "3352e41cc30b40ae80108970492b21014049e625"),
        (1024, "0ac28084ff74933d05123496dafd3791684d9b53"),
        (65536, "fefb71740a82b94a2da3bcd2fd72fc64a7fb8666"),
    ] {
        let bytes = (0..len).map(|i| (i % 251) as u8).collect();
        assert_eq!(hex::encode(digest_owned(bytes)), expected, "length {len}");
    }
}

#[test]
fn git_framing_and_existing_buffer_capacity_do_not_change_the_digest() {
    for (bytes, expected) in [
        (b"".as_slice(), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
        (
            b"hello\n".as_slice(),
            "ce013625030ba8dba906f756967f9e9ca394464a",
        ),
    ] {
        let mut message = format!("blob {}\0", bytes.len()).into_bytes();
        message.extend_from_slice(bytes);
        let mut spare = Vec::with_capacity(message.len() + 4096);
        spare.extend_from_slice(&message);
        assert_eq!(hex::encode(digest_owned(message)), expected);
        assert_eq!(hex::encode(digest_owned(spare)), expected);
    }
}
