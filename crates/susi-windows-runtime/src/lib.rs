//! Original Rust startup for the qualified x64 Windows compiler/OS boundary.
//!
//! This crate retains the compiler-generated C `main` and Rust runtime setup.
//! It provides PE TLS metadata and executable-local initializer/terminator
//! traversal without linking Microsoft CRT implementation objects. OS imports
//! remain external platform services. See this crate's README for qualification.
#![no_std]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(all(windows, not(all(target_arch = "x86_64", target_env = "msvc"))))]
compile_error!("susi-windows-runtime requires separately qualified x64 Windows ABI");

#[cfg(all(windows, target_arch = "x86_64", not(test)))]
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
#[cfg_attr(not(all(windows, target_arch = "x86_64")), allow(dead_code))]
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
}
