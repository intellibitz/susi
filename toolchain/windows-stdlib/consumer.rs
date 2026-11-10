//! Run with the private sysroot and ordinary stable rustc (no bootstrap).
#[test]
fn platform_unicode_and_standard_library_behave_on_real_inputs() {
    // Keep this first: all workers race the backend's initial loader publication.
    // A separate libtest could run after a previous test had already loaded ICU.
    #[cfg(target_os = "linux")]
    assert!(
        !std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .contains("/libicuuc.so."),
        "Unicode backend loaded before the first-use race"
    );
    let start = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|index| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                match index % 3 {
                    0 => assert!('δ'.is_alphabetic()),
                    1 => assert!('¾'.is_numeric()),
                    _ => assert_eq!('δ'.to_uppercase().next(), Some('Δ')),
                }
                assert_eq!("Straße ﬃ".to_uppercase(), "STRASSE FFI");
                assert_eq!("İI".to_lowercase(), "i\u{307}i");
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    #[cfg(windows)]
    assert_eq!(char::UNICODE_VERSION, (15, 1, 0));
    // Linux selects the newest qualified OS ICU present; the constant is the
    // oldest qualified repertoire. Observe the library this process loaded.
    #[cfg(target_os = "linux")]
    assert_eq!(char::UNICODE_VERSION, (15, 0, 0));
    #[cfg(target_os = "linux")]
    let icu_major = {
        assert!('δ'.is_alphabetic());
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let loaded: Vec<u8> = [78, 76, 74, 72]
            .into_iter()
            .filter(|major| maps.contains(&format!("/libicuuc.so.{major}")))
            .collect();
        assert_eq!(loaded.len(), 1, "exactly one qualified ICU must be loaded");
        loaded[0]
    };
    assert_eq!("Straße ﬃ".to_uppercase(), "STRASSE FFI");
    assert_eq!("İI".to_lowercase(), "i\u{307}i");
    assert_eq!("𐐀".to_lowercase(), "𐐨");
    assert_eq!("\u{a0} useful \u{3000}".trim(), "useful");
    assert!('¾'.is_numeric());
    assert!(!'一'.is_numeric());
    assert_eq!('\n'.escape_debug().to_string(), "\\n");
    #[cfg(windows)]
    assert_eq!('\u{1c89}'.escape_debug().to_string(), "\\u{1c89}");
    // U+1C89 is assigned in Unicode 16.0 (ICU 76 and newer) and unassigned in 15.1 / 15.0.
    #[cfg(target_os = "linux")]
    assert_eq!(
        '\u{1c89}'.escape_debug().to_string(),
        if matches!(icu_major, 76 | 78) {
            "\u{1c89}"
        } else {
            "\\u{1c89}"
        }
    );
    // U+1E6C0 TAI YO LETTER LOW KO was assigned in Unicode 17.0.
    #[cfg(target_os = "linux")]
    {
        assert_eq!('\u{1e6c0}'.is_alphabetic(), icu_major == 78);
        assert_eq!(
            '\u{1e6c0}'.escape_debug().to_string(),
            if icu_major == 78 {
                "\u{1e6c0}"
            } else {
                "\\u{1e6c0}"
            }
        );
    }
    assert_eq!('δ'.escape_debug().to_string(), "δ");
    assert_eq!(
        char::decode_utf16([0xd800])
            .next()
            .unwrap()
            .unwrap_err()
            .unpaired_surrogate(),
        0xd800
    );
    assert_eq!(
        char::decode_utf16([0xd801, 0xdc00])
            .next()
            .unwrap()
            .unwrap(),
        '𐐀'
    );
    let worker = std::thread::spawn(|| {
        let mut map = std::collections::HashMap::new();
        map.insert("δοκιμή".to_uppercase(), 42);
        map["ΔΟΚΙΜΉ"]
    });
    assert_eq!(worker.join().unwrap(), 42);
    assert!(std::panic::catch_unwind(|| panic!("qualified unwind")).is_err());
}
