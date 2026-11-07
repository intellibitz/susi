//! Notify already-running Windows shells after persisting a per-user PATH.
//! Owned by the Win32 facade since 2026-11-07: the open-core boundary keeps
//! `susi-config` free of host-integration helpers, and the root binary and
//! `susi-harness` forbid `unsafe_code`.
//! The timeout applies per top-level recipient, not to the whole broadcast.
//! https://learn.microsoft.com/windows/win32/winmsg/wm-settingchange
use std::io;

#[allow(unsafe_code)]
pub fn notify_environment_changed() -> io::Result<()> {
    // SAFETY: This is the documented, synchronous Win32 SendMessageTimeoutW
    // boundary. HWND_BROADCAST is the defined pseudo-handle, WPARAM is zero,
    // and LPARAM refers only to the static, NUL-terminated UTF-16 "Environment"
    // string. Neither calling code nor a timed-out recipient can outlive that
    // allocation. No result pointer is supplied; no user pointer crosses the ABI.
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendMessageTimeoutW(
            window: *mut std::ffi::c_void,
            message: u32,
            wparam: usize,
            lparam: isize,
            flags: u32,
            timeout_ms: u32,
            result: *mut usize,
        ) -> isize;
    }
    static ENVIRONMENT: [u16; 12] = [69, 110, 118, 105, 114, 111, 110, 109, 101, 110, 116, 0];
    // SAFETY: The only inputs are the ABI constants and static buffer described
    // above. SMTO_ABORTIFHUNG bounds each recipient's wait independently.
    let sent = unsafe {
        SendMessageTimeoutW(
            0xffffusize as *mut _,
            0x001a,
            0,
            ENVIRONMENT.as_ptr() as isize,
            2,
            1000,
            std::ptr::null_mut(),
        )
    };
    if sent == 0 {
        // The documented failure/timeout path need not set GetLastError.
        return Err(io::Error::other(
            "Windows did not confirm the environment broadcast to all recipients",
        ));
    }
    Ok(())
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn notify_environment_changed_returns_ok_or_descriptive_io_error() {
        if let Err(err) = notify_environment_changed() {
            assert_eq!(err.kind(), io::ErrorKind::Other);
            assert!(err.to_string().contains("environment broadcast"));
        }
    }

    #[test]
    fn notify_environment_changed_is_safe_across_repeated_calls() {
        let first = notify_environment_changed();
        let second = notify_environment_changed();
        if let Err(e) = first {
            assert_eq!(e.kind(), io::ErrorKind::Other);
        }
        if let Err(e) = second {
            assert_eq!(e.kind(), io::ErrorKind::Other);
        }
    }

    #[test]
    fn environment_utf16_slice_decodes_to_null_terminated_environment_string() {
        let raw: [u16; 12] = [69, 110, 118, 105, 114, 111, 110, 109, 101, 110, 116, 0];
        let decoded = String::from_utf16(&raw[..11]).expect("valid utf16");
        assert_eq!(decoded, "Environment");
        assert_eq!(raw[11], 0);
    }

    #[test]
    fn broadcast_timeout_error_preserves_other_error_kind_and_message() {
        let err =
            io::Error::other("Windows did not confirm the environment broadcast to all recipients");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(err.to_string().contains("Windows did not confirm"));
    }

    #[test]
    fn environment_utf16_matches_encode_utf16_of_environment_plus_nul() {
        let mut expected: Vec<u16> = "Environment".encode_utf16().collect();
        expected.push(0);
        assert_eq!(
            expected.as_slice(),
            &[69u16, 110, 118, 105, 114, 111, 110, 109, 101, 110, 116, 0]
        );
    }
}
