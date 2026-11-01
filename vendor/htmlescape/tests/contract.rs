use htmlescape::encode_minimal;
#[test]
fn exact_entities_and_unicode_preservation() {
    assert_eq!(encode_minimal("\"&'<>"), "&quot;&amp;&#x27;&lt;&gt;");
    assert_eq!(encode_minimal(""), "");
    assert_eq!(
        encode_minimal("café λ 中 😀 e\u{301}\0\r\n"),
        "café λ 中 😀 e\u{301}\0\r\n"
    );
    assert_eq!(
        encode_minimal("&lt;<b title=\"a'b\">"),
        "&amp;lt;&lt;b title=&quot;a&#x27;b&quot;&gt;"
    );
}
#[test]
fn only_five_scalars_change() {
    for value in 0..=0x10ffff {
        if let Some(ch) = char::from_u32(value) {
            let input = ch.to_string();
            if !matches!(ch, '"' | '&' | '\'' | '<' | '>') {
                assert_eq!(encode_minimal(&input), input, "U+{value:04X}");
            }
        }
    }
}
