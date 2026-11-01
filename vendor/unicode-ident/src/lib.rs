//! Independent identifier predicates without distributed Unicode tables.
//! ASCII is classified directly. Macros ask their running compiler. Other
//! callers use OS properties, with a bounded compiler helper for newer scalars.
//! Checked APIs distinguish missing capability from a negative classification.
#![forbid(unsafe_code)]
extern crate proc_macro;
mod compiler;
use std::cell::RefCell;

/// No fixed table is bundled. Zero avoids claiming a fixed Unicode version.
/// `platform_unicode_version` reports the OS version; macros follow rustc.
pub const UNICODE_VERSION: (u8, u8, u8) = (0, 0, 0);
thread_local! {
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Compatibility predicate. Missing capability fails closed and is retained
/// by `take_classification_error`; source-analysis callers must inspect it.
pub fn is_xid_start(ch: char) -> bool {
    compatibility_result(try_is_xid_start(ch))
}
/// Compatibility predicate with the same error reporting as `is_xid_start`.
pub fn is_xid_continue(ch: char) -> bool {
    compatibility_result(try_is_xid_continue(ch))
}
pub fn try_is_xid_start(ch: char) -> Result<bool, String> {
    classify(ch, false)
}
pub fn try_is_xid_continue(ch: char) -> Result<bool, String> {
    classify(ch, true)
}
pub fn clear_classification_error() {
    LAST_ERROR.with(|error| *error.borrow_mut() = None);
}
pub fn take_classification_error() -> Option<String> {
    LAST_ERROR.with(|error| error.borrow_mut().take())
}
pub fn platform_unicode_version() -> Result<[u8; 4], String> {
    idna_adapter::platform_status()
        .map(|status| status.unicode_version)
        .map_err(|error| error.to_string())
}
/// Explicit process teardown for hosts that have completed source analysis.
/// The service also shuts down automatically after ten seconds without queries.
pub fn shutdown_compiler() -> Result<(), String> {
    compiler::shutdown().map_err(|error| error.to_string())
}
fn compatibility_result(result: Result<bool, String>) -> bool {
    match result {
        Ok(value) => value,
        Err(message) => {
            LAST_ERROR.with(|error| {
                error.borrow_mut().get_or_insert(message);
            });
            false
        }
    }
}
fn identifier(ch: char, continuation: bool) -> String {
    if continuation {
        format!("a{ch}a")
    } else {
        ch.to_string()
    }
}
fn classify(ch: char, continuation: bool) -> Result<bool, String> {
    if ch.is_ascii() {
        return Ok(ch.is_ascii_alphabetic() || (continuation && (ch.is_ascii_digit() || ch == '_')));
    }
    if ch.is_whitespace() || ch.is_control() {
        return Ok(false);
    }
    if proc_macro::is_available() {
        // TokenStream parsing can poison compilation on negative queries.
        // Ident's documented invalid-input panic is caught without hook changes.
        return Ok(std::panic::catch_unwind(|| {
            proc_macro::Ident::new(&identifier(ch, continuation), proc_macro::Span::call_site())
        })
        .is_ok());
    }
    if idna_adapter::platform_is_assigned(ch).is_ok_and(|assigned| assigned) {
        let result = if continuation {
            idna_adapter::platform_xid_continue(ch)
        } else {
            idna_adapter::platform_xid_start(ch)
        };
        return result.map_err(|error| error.to_string());
    }
    compiler::classify(ch, continuation).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ascii_and_multilingual_identifiers_keep_xid_distinctions() {
        assert!(is_xid_start('A'));
        assert!(!is_xid_start('_'));
        assert!(is_xid_continue('_'));
        assert!(!is_xid_start('7'));
        assert!(is_xid_continue('7'));
        for ch in ['é', 'λ', '中', 'Ж'] {
            assert!(is_xid_start(ch), "{ch:?}");
            assert!(is_xid_continue(ch), "{ch:?}");
        }
        assert!(!is_xid_start('\u{301}'));
        assert!(is_xid_continue('\u{301}'));
        for ch in [' ', '\u{85}', '\u{2003}', '\u{2028}', '\u{feff}', '😀'] {
            assert!(!is_xid_start(ch), "{ch:?}");
            assert!(!is_xid_continue(ch), "{ch:?}");
        }
        assert!(take_classification_error().is_none());
    }
    #[test]
    fn compiler_handles_identifiers_newer_than_system_unicode() {
        let _fixture = compiler::TEST_SERIAL
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Newer than the qualification host's OS Unicode repertoire.
        assert!(try_is_xid_start('\u{105c0}').unwrap());
        assert!(try_is_xid_continue('\u{105c0}').unwrap());
        shutdown_compiler().unwrap();
    }
    #[test]
    fn capability_errors_are_observable_and_thread_local() {
        clear_classification_error();
        assert!(!compatibility_result(Err("compiler unavailable".into())));
        std::thread::spawn(|| assert!(take_classification_error().is_none()))
            .join()
            .unwrap();
        assert_eq!(
            take_classification_error().as_deref(),
            Some("compiler unavailable")
        );
        assert!(take_classification_error().is_none());
    }
}
