//! Identity of an open local unnamed file stream. Unavailable identity disables reuse.
use std::{ffi::c_void, fs::File, os::windows::io::AsRawHandle};

#[repr(C)]
#[derive(Debug, Default, Eq, PartialEq)]
struct FileId {
    volume: u64,
    id: [u8; 16],
}

#[derive(Debug, Eq, PartialEq)]
pub struct ModelFileIdentity {
    volume_guid: [u16; 36],
    file: FileId,
}

#[allow(unsafe_code)] // Narrow OS boundary for identity of an already-open borrowed file.
pub fn model_file_identity(file: &File) -> Option<ModelFileIdentity> {
    // SAFETY: These are the documented Kernel32 ABI declarations. FILE_ID_INFO
    // is one ULONGLONG followed by 16 bytes. WCHAR counts are in UTF-16 units.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            handle: *mut c_void,
            class: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
        fn GetFinalPathNameByHandleW(
            handle: *mut c_void,
            name: *mut u16,
            length: u32,
            flags: u32,
        ) -> u32;
    }
    const FILE_ID_INFO: i32 = 0x12;
    const NORMALIZED_VOLUME_GUID: u32 = 1;
    const MAX_PATH_UNITS: usize = 32768;
    let mut id = FileId::default();
    // SAFETY: file remains open; id is a writable FILE_ID_INFO with the exact
    // ABI size. Windows consumes the pointer only during this call.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FILE_ID_INFO,
            (&mut id as *mut FileId).cast(),
            std::mem::size_of::<FileId>() as u32,
        )
    };
    if ok == 0 || id.id == [0; 16] {
        return None;
    }

    let read_name = |buffer: &mut [u16]| -> usize {
        // SAFETY: both caller buffers are at most MAX_PATH_UNITS WCHARs.
        // Windows reports the required size instead of overrunning the buffer.
        unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                NORMALIZED_VOLUME_GUID,
            ) as usize
        }
    };
    let mut short = [0u16; 512];
    let mut size = read_name(&mut short);
    let mut long;
    let path = if size < short.len() {
        &short[..size]
    } else {
        if size > MAX_PATH_UNITS {
            return None;
        }
        long = vec![0u16; size];
        size = read_name(&mut long);
        if size >= long.len() {
            return None;
        }
        &long[..size]
    };
    Some(ModelFileIdentity {
        volume_guid: volume_guid(path)?,
        file: id,
    })
}

fn volume_guid(path: &[u16]) -> Option<[u16; 36]> {
    const PREFIX: &str = r"\\?\Volume{";
    if !path
        .iter()
        .copied()
        .take(PREFIX.len())
        .eq(PREFIX.encode_utf16())
        || path.get(PREFIX.len() + 36..PREFIX.len() + 38) != Some(&[b'}' as u16, b'\\' as u16])
        || path.contains(&(b':' as u16))
    {
        return None;
    }
    // File IDs identify the file, not its named NTFS streams. Only the
    // normalized local volume path of the actual handle qualifies for reuse.
    let guid: [u16; 36] = path.get(PREFIX.len()..PREFIX.len() + 36)?.try_into().ok()?;
    for (i, &unit) in guid.iter().enumerate() {
        let ch = u8::try_from(unit).ok()?;
        if matches!(i, 8 | 13 | 18 | 23) {
            if ch != b'-' {
                return None;
            }
        } else if !ch.is_ascii_hexdigit() {
            return None;
        }
    }
    Some(guid)
}

#[cfg(test)]
mod tests {
    use super::volume_guid;

    #[test]
    fn only_normalized_unnamed_local_volume_paths_qualify() {
        let local = r"\\?\Volume{12345678-1234-ABCD-9876-aabbccddeeff}\m\weights.gguf";
        let parsed = |s: &str| volume_guid(&s.encode_utf16().collect::<Vec<_>>());
        assert!(parsed(local).is_some());
        for rejected in [
            local.replace("ABCD", "ABCG"),
            local.replace("-1234", "_1234"),
            format!("{local}:stream"),
            r"\\?\UNC\server\share\weights.gguf".into(),
            r"C:\models\weights.gguf".into(),
        ] {
            assert!(parsed(&rejected).is_none());
        }
    }
}
