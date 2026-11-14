//! Publish a fully written staging file without a delete-pending interval at
//! the destination. Open readers may finish reading the old file.
#![allow(unsafe_code)]

use std::fs::OpenOptions;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use winapi::shared::winerror::{
    ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED,
};
use winapi::um::fileapi::{SetFileInformationByHandle, FILE_RENAME_INFO};
use winapi::um::minwinbase::FileRenameInfoEx;
use winapi::um::winbase::FILE_FLAG_OPEN_REPARSE_POINT;
use winapi::um::winnt::DELETE;

pub fn replace_staged(staged: &Path, destination: &Path) -> io::Result<()> {
    const REPLACE_IF_EXISTS: u32 = 1;
    const POSIX_SEMANTICS: u32 = 2;

    let destination = std::path::absolute(destination)?;
    let name: Vec<u16> = destination.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let name_bytes = u32::try_from(std::mem::size_of_val(name.as_slice()))
        .map_err(|_| io::ErrorKind::InvalidInput)?;
    let name_offset = offset_of!(FILE_RENAME_INFO, FileName);
    let buffer_bytes =
        (name_offset + name_bytes as usize + size_of::<u16>()).max(size_of::<FILE_RENAME_INFO>());
    let size = u32::try_from(buffer_bytes).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut buffer = vec![0usize; buffer_bytes.div_ceil(size_of::<usize>())];
    let file = OpenOptions::new()
        .access_mode(DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(staged)?;

    // SAFETY: the zeroed buffer is pointer-aligned and spans the C header,
    // UTF-16 path and trailing NUL. The size/name offsets come from the ABI.
    // RootDirectory stays null for the absolute path. The handle and buffer
    // remain live through the synchronous call.
    let ok = unsafe {
        let bytes = std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), buffer_bytes);
        bytes[..4].copy_from_slice(&(REPLACE_IF_EXISTS | POSIX_SEMANTICS).to_ne_bytes());
        let length_offset = offset_of!(FILE_RENAME_INFO, FileNameLength);
        bytes[length_offset..length_offset + 4].copy_from_slice(&name_bytes.to_ne_bytes());
        for (unit, pair) in name.iter().zip(bytes[name_offset..].as_chunks_mut::<2>().0) {
            pair.copy_from_slice(&unit.to_ne_bytes());
        }
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileRenameInfoEx,
            bytes.as_mut_ptr().cast(),
            size,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // Some file systems only implement the legacy rename operation.
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_INVALID_FUNCTION | ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED) => {
            drop(file);
            std::fs::rename(staged, destination)
        }
        _ => Err(error),
    }
}
