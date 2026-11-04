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
}
