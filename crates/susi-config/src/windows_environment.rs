//! Notify already-running Windows shells after persisting a per-user PATH.
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
