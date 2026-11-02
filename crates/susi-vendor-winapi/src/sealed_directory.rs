//! A held directory permits lookup, but no ordinary principal can create a
//! child or rewrite its DACL. Its owner can delete the empty directory for GC
//! only after our deny-delete handle is released. OS administrators remain the
//! platform trust boundary.
use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
};
use winapi::um::{
    securitybaseapi::{GetKernelObjectSecurity, SetKernelObjectSecurity},
    winnt::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, READ_CONTROL, WRITE_DAC,
    },
};

// Self-relative Windows SECURITY_DESCRIPTOR for
// D:P(D;;WDWO;;;OW)(A;;SD;;;OW)(A;;FRFX;;;WD).
// Owner Rights explicitly removes the owner's implicit WRITE_DAC. Only read,
// execute and the owner's delete right are granted. No inheritable ACEs.
#[repr(align(4))]
struct Aligned<const N: usize>([u8; N]);
const DESCRIPTOR: Aligned<88> = Aligned([
    1, 0, 4, 144, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 2, 0, 68, 0, 3, 0, 0, 0, 1, 0,
    20, 0, 0, 0, 12, 0, 1, 1, 0, 0, 0, 0, 0, 3, 4, 0, 0, 0, 0, 0, 20, 0, 0, 0, 1, 0, 1, 1, 0, 0, 0,
    0, 0, 3, 4, 0, 0, 0, 0, 0, 20, 0, 169, 0, 18, 0, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
]);

fn open(path: &Path, rights: u32) -> io::Result<File> {
    let file = OpenOptions::new()
        .access_mode(rights)
        .share_mode(1)
        .custom_flags(0x02000000 | 0x00200000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
        return Err(io::Error::other(
            "namespace guard requires a real directory",
        ));
    }
    Ok(file)
}

/// Install only on a newly created private cache directory.
#[allow(unsafe_code)] // SAFETY: narrowly scoped OS ACL operation on a held directory.
pub fn seal_new(path: &Path) -> io::Result<()> {
    let file = open(path, WRITE_DAC | READ_CONTROL)?;
    // SAFETY: file owns the live handle; the aligned constant is a complete
    // self-relative descriptor, and the API only reads it during this call.
    let ok = unsafe {
        SetKernelObjectSecurity(
            file.as_raw_handle().cast(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            DESCRIPTOR.0.as_ptr().cast_mut().cast(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(file);
    hold(path)?;
    Ok(())
}

/// Check the descriptor on the same handle that denies deletion through use.
/// The exact ACL is checked even for previously admitted objects.
#[allow(unsafe_code)] // SAFETY: bounded OS descriptor read on an owned directory handle.
pub fn hold(path: &Path) -> io::Result<File> {
    let file = open(path, READ_CONTROL)?;
    let mut buf = Aligned([0u8; 512]);
    let mut needed = 0;
    // SAFETY: the aligned writable buffer and size agree, and needed points to
    // a live u32. The handle and both outputs remain valid throughout the call.
    let ok = unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle().cast(),
            DACL_SECURITY_INFORMATION,
            buf.0.as_mut_ptr().cast(),
            buf.0.len() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let data = buf
        .0
        .get(..needed as usize)
        .ok_or_else(|| io::Error::other("invalid descriptor length"))?;
    let header = data
        .get(..20)
        .ok_or_else(|| io::Error::other("truncated descriptor"))?;
    let flags = u16::from_le_bytes([header[2], header[3]]);
    let offset = u32::from_le_bytes([header[16], header[17], header[18], header[19]]) as usize;
    if header[0] != 1
        || flags & 0x9004 != 0x9004
        || data.get(offset..).and_then(|acl| acl.get(..68)) != Some(&DESCRIPTOR.0[20..])
    {
        return Err(io::Error::other("unsealed stdlib directory namespace"));
    }
    Ok(file)
}

/// Latched notification. Detect a transient write, rename or DACL change even
/// if a previously opened privileged handle restores the original bytes/ACL.
/// Never rearm a signaled handle or accept output from that compiler invocation.
pub struct Changes(winapi::um::winnt::HANDLE);
impl Changes {
    #[allow(unsafe_code)] // SAFETY: bounded NUL-terminated UTF-16 path for a native notification.
    pub fn watch(path: &Path) -> io::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use winapi::um::{
            fileapi::FindFirstChangeNotificationW,
            handleapi::INVALID_HANDLE_VALUE,
            winnt::{
                FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
                FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SECURITY,
                FILE_NOTIFY_CHANGE_SIZE,
            },
        };
        let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
        if name.contains(&0) {
            return Err(io::Error::other("invalid watch path"));
        }
        name.push(0);
        // SAFETY: name remains valid through the call; the filter and subtree
        // flag are documented values. Only a valid handle is handed to Drop.
        let handle = unsafe {
            FindFirstChangeNotificationW(
                name.as_ptr(),
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_SECURITY
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }
    #[allow(unsafe_code)] // SAFETY: poll the owned, non-resetting kernel handle.
    pub fn unchanged(&self) -> io::Result<()> {
        // SAFETY: self owns a live change-notification handle, and zero timeout
        // polls without acknowledging a signaled notification.
        match unsafe { winapi::um::synchapi::WaitForSingleObject(self.0, 0) } {
            258 => Ok(()), // WAIT_TIMEOUT
            0 => Err(io::Error::other("stdlib namespace changed during use")),
            _ => Err(io::Error::last_os_error()),
        }
    }
}
impl Drop for Changes {
    #[allow(unsafe_code)] // SAFETY: release exactly this object's change-notification handle.
    fn drop(&mut self) {
        // SAFETY: ownership is unique, and FindCloseChangeNotification is the
        // matching release function. Drop cannot retry or propagate an error.
        unsafe {
            winapi::um::fileapi::FindCloseChangeNotification(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restored_namespace_still_invalidates_the_operation() -> io::Result<()> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("stdlib-transient-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir)?;
        let changes = Changes::watch(&dir)?;
        changes.unchanged()?;
        let extra = dir.join("transient.rlib");
        std::fs::write(&extra, b"unadmitted")?;
        std::fs::remove_file(&extra)?;
        assert!(!extra.exists());
        assert!(changes.unchanged().is_err());
        assert!(changes.unchanged().is_err());
        drop(changes);
        std::fs::remove_dir(dir)
    }
}
