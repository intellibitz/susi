//! Original Rust startup for the native Windows MSVC compiler/OS boundary.
//! The allow-list is explicit: x86_64 (qualified) and aarch64 (admitted only so
//! a native ARM64 host can produce its own acceptance evidence; not qualified
//! until that evidence exists). Every other Windows target fails to compile.
//!
//! This crate retains the compiler-generated C `main` and Rust runtime setup.
//! It provides PE TLS metadata and executable-local initializer/terminator
//! traversal without linking Microsoft CRT implementation objects. OS imports
//! remain external platform services. See this crate's README for qualification.
#![no_std]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate std;

#[cfg(all(
    windows,
    not(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_env = "msvc"
    ))
))]
compile_error!(
    "susi-windows-runtime requires a separately qualified x86_64 or aarch64 Windows MSVC ABI"
);

#[cfg(all(
    windows,
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(test)
))]
mod platform;

#[cfg(all(
    windows,
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_env = "msvc"
))]
// SAFETY: declarations match the target's Windows system ABI.
#[allow(unsafe_code)]
#[link(name = "kernel32")]
extern "system" {
    fn GetHandleInformation(object: *mut core::ffi::c_void, flags: *mut u32) -> i32;
    fn SetHandleInformation(object: *mut core::ffi::c_void, mask: u32, flags: u32) -> i32;
}

/// Spawn a detached background command without letting `CreateProcessW`'s
/// `bInheritHandles = TRUE` (enabled by `Stdio::null()`) copy the parent
/// CLI's own stdin/stdout/stderr pipe handles into the long-lived daemon.
/// Without clearing `HANDLE_FLAG_INHERIT` on the parent's std handles
/// around `spawn()`, any caller reading `susi`'s stdout/stderr pipe
/// (`Command::output()`, release smoke missions, `| Out-Null`) blocks
/// forever waiting for EOF after the CLI exits whenever `susi` started or
/// restarted the daemon.
// SAFETY: the function-local proof documents each qualified ABI operation.
#[allow(unsafe_code)]
pub fn spawn_without_inheriting_stdio(
    cmd: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    #[cfg(all(
        windows,
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_env = "msvc"
    ))]
    {
        use std::os::windows::io::AsRawHandle;

        const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
        let raw_handles: [*mut core::ffi::c_void; 3] = [
            std::io::stdin().as_raw_handle(),
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ];
        let mut restore: std::vec::Vec<(*mut core::ffi::c_void, u32)> =
            std::vec::Vec::with_capacity(3);
        for handle in raw_handles {
            if handle.is_null() {
                continue;
            }
            let mut flags: u32 = 0;
            // SAFETY: `GetHandleInformation` and `SetHandleInformation`
            // inspect and toggle the `HANDLE_FLAG_INHERIT` bit on the
            // current process's standard handles; invalid or pseudo-handles
            // simply return FALSE and are skipped.
            unsafe {
                if GetHandleInformation(handle, &mut flags) != 0
                    && (flags & HANDLE_FLAG_INHERIT) != 0
                {
                    let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                    restore.push((handle, flags & HANDLE_FLAG_INHERIT));
                }
            }
        }
        let result = cmd.spawn();
        for (handle, orig_inherit) in restore {
            // SAFETY: restores the original `HANDLE_FLAG_INHERIT` bit on
            // the same valid standard handle captured above.
            unsafe {
                let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, orig_inherit);
            }
        }
        result
    }
    #[cfg(not(all(
        windows,
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_env = "msvc"
    )))]
    {
        cmd.spawn()
    }
}

/// A malformed executable initializer range is rejected before reading slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeError {
    Overflow,
    OutsideImage,
    Misaligned,
    TooLarge,
}

/// PE sections are loader-owned mapped memory, not a Rust allocation spanning
/// the separately emitted marker statics. Keep all traversal arithmetic checked
/// and integral; platform code obtains exposed-provenance pointers per slot.
#[cfg_attr(
    not(all(windows, any(target_arch = "x86_64", target_arch = "aarch64"))),
    allow(dead_code)
)]
fn table_slots(
    base: usize,
    image_size: usize,
    start: usize,
    end: usize,
) -> Result<usize, RangeError> {
    let limit = base.checked_add(image_size).ok_or(RangeError::Overflow)?;
    let end_of_marker = end
        .checked_add(core::mem::size_of::<usize>())
        .ok_or(RangeError::Overflow)?;
    if start < base || end < start || end_of_marker > limit {
        return Err(RangeError::OutsideImage);
    }
    let width = core::mem::size_of::<usize>();
    if !start.is_multiple_of(width) || !end.is_multiple_of(width) {
        return Err(RangeError::Misaligned);
    }
    let count = (end - start) / width;
    if count > 16_384 {
        return Err(RangeError::TooLarge);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linker_ranges_are_bounded_and_aligned() {
        assert_eq!(table_slots(0x1000, 0x1000, 0x1100, 0x1118), Ok(3));
        assert_eq!(table_slots(0x1000, 0x1000, 0x1100, 0x1100), Ok(0));
        assert_eq!(
            table_slots(0x1000, 0x1000, 0x0ff8, 0x1100),
            Err(RangeError::OutsideImage)
        );
        assert_eq!(
            table_slots(0x1000, 0x1000, 0x1100, 0x2000),
            Err(RangeError::OutsideImage)
        );
        assert_eq!(
            table_slots(0x1000, 0x1000, 0x1101, 0x1118),
            Err(RangeError::Misaligned)
        );
        assert_eq!(table_slots(usize::MAX, 2, 0, 0), Err(RangeError::Overflow));
        assert_eq!(
            table_slots(0, usize::MAX, 0, 0x40000),
            Err(RangeError::TooLarge)
        );
    }

    #[test]
    fn table_slots_enforces_inverted_range_end_overflow_end_alignment_and_exact_slot_ceiling() {
        let width = core::mem::size_of::<usize>();
        assert_eq!(
            table_slots(0x1000, 0x1000, 0x1200, 0x1100),
            Err(RangeError::OutsideImage)
        );
        assert_eq!(
            table_slots(0, usize::MAX, 0x1000, usize::MAX),
            Err(RangeError::Overflow)
        );
        assert_eq!(
            table_slots(0x1000, 0x1000, 0x1100, 0x1117),
            Err(RangeError::Misaligned)
        );
        assert_eq!(
            table_slots(0, usize::MAX, 0x1000, 0x1000 + 16_384 * width),
            Ok(16_384)
        );
        assert_eq!(
            table_slots(0, usize::MAX, 0x1000, 0x1000 + 16_385 * width),
            Err(RangeError::TooLarge)
        );
    }

    #[test]
    fn table_slots_enforces_exact_image_boundaries_including_end_marker_width() {
        let width = core::mem::size_of::<usize>();
        let base = 0x4000;
        let image_size = 0x1000;
        let limit = base + image_size;
        assert_eq!(
            table_slots(base, image_size, base, limit - width),
            Ok((image_size - width) / width)
        );
        assert_eq!(
            table_slots(base, image_size, base, limit),
            Err(RangeError::OutsideImage)
        );
    }

    #[test]
    fn range_error_traits_and_single_slot_table_calculation() {
        let width = core::mem::size_of::<usize>();
        assert_eq!(table_slots(0x2000, 0x100, 0x2000, 0x2000 + width), Ok(1));

        let err = RangeError::Misaligned;
        let copied = err;
        assert_eq!(err, copied);
        assert_ne!(err, RangeError::Overflow);
        assert_ne!(err, RangeError::OutsideImage);
        assert_ne!(err, RangeError::TooLarge);
    }

    #[test]
    fn spawn_without_inheriting_stdio_spawns_and_waits_cleanly() -> std::io::Result<()> {
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--help")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = spawn_without_inheriting_stdio(&mut cmd)?;
        let status = child.wait()?;
        assert!(status.success());
        Ok(())
    }
}
