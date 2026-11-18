use super::*;
use tokio::io::AsyncReadExt;
#[tokio::test]
async fn limits_individual_lines_including_fragmented_reads() {
    let mut reader = BoundedLines::new(&b"123\n123\n"[..], 3);
    let mut output = Vec::new();
    reader.read_to_end(&mut output).await.unwrap();
    assert_eq!(output, b"123\n123\n");
    let mut reader = BoundedLines::new(&b"12345\n"[..], 3);
    let mut first = [0; 2];
    reader.read_exact(&mut first).await.unwrap();
    assert!(reader.read_exact(&mut first).await.is_err());
}

#[tokio::test]
async fn recovers_after_the_oversized_line() {
    // Trip and terminator in the same chunk: the next line survives.
    let mut reader = BoundedLines::new(&b"12345\nok\n"[..], 3);
    let mut buf = [0u8; 16];
    assert!(reader.read(&mut buf).await.is_err());
    let mut line = String::new();
    reader.read_to_string(&mut line).await.unwrap();
    assert_eq!(line, "ok\n");

    // Trip and terminator in different chunks: same recovery.
    let mut reader = BoundedLines::new(&b"1234567890\nok"[..], 3);
    let mut recovered = Vec::new();
    loop {
        match reader.read(&mut buf).await {
            Err(_) => {}
            Ok(0) => break,
            Ok(n) => recovered.extend_from_slice(&buf[..n]),
        }
    }
    assert_eq!(recovered, b"ok");

    // A stashed line is still bounded.
    let mut reader = BoundedLines::new(&b"12345\n6789\nok\n"[..], 3);
    let mut recovered = Vec::new();
    let mut errors = 0;
    loop {
        match reader.read(&mut buf).await {
            Err(_) => errors += 1,
            Ok(0) => break,
            Ok(n) => recovered.extend_from_slice(&buf[..n]),
        }
        if errors > 8 {
            panic!("recovery must terminate");
        }
    }
    assert!(errors >= 2, "both oversized lines must error");
    assert_eq!(recovered, b"ok\n");
}

#[tokio::test]
async fn bounded_lines_allows_line_exactly_at_byte_limit() {
    let mut reader = BoundedLines::new(&b"abcd\nef\n"[..], 4);
    let mut output = Vec::new();
    reader.read_to_end(&mut output).await.unwrap();
    assert_eq!(output, b"abcd\nef\n");
}

#[tokio::test]
async fn bounded_lines_handles_empty_input_cleanly() {
    let mut reader = BoundedLines::new(&b""[..], 8);
    let mut output = Vec::new();
    let n = reader.read_to_end(&mut output).await.unwrap();
    assert_eq!(n, 0);
    assert!(output.is_empty());
}

#[test]
fn oversized_helper_constructs_invalid_data_error_with_limit_message() {
    let err = BoundedLines::<&[u8]>::oversized(false);
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("byte limit"));
}
