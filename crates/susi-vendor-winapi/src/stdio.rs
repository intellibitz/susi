//! Isolate the caller's standard handles when launching a background child.

/// Start a child with its configured stdio without also handing it the caller's
/// inheritable stdin, stdout and stderr as unrelated Win32 handles.
#[allow(unsafe_code)]
pub fn spawn_without_inheriting_stdio(
    cmd: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    use std::os::windows::io::AsRawHandle;
    use winapi::shared::minwindef::DWORD;
    use winapi::um::handleapi::{GetHandleInformation, SetHandleInformation};
    use winapi::um::winnt::HANDLE;

    const HANDLE_FLAG_INHERIT: DWORD = 0x0000_0001;
    // Multiple desktop operations can spawn concurrently. Keep the flags cleared
    // until this spawn has inherited its explicitly redirected stdio.
    static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let raw_handles: [HANDLE; 3] = [
        std::io::stdin().as_raw_handle() as HANDLE,
        std::io::stdout().as_raw_handle() as HANDLE,
        std::io::stderr().as_raw_handle() as HANDLE,
    ];
    let mut restore: Vec<(HANDLE, DWORD)> = Vec::with_capacity(3);
    for handle in raw_handles {
        if handle.is_null() {
            continue;
        }
        let mut flags: DWORD = 0;
        // SAFETY: `GetHandleInformation` and `SetHandleInformation`
        // inspect and toggle the `HANDLE_FLAG_INHERIT` bit on the
        // current process's standard handles; invalid or pseudo-handles
        // simply return FALSE and are skipped.
        unsafe {
            if GetHandleInformation(handle, &mut flags) != 0 && (flags & HANDLE_FLAG_INHERIT) != 0 {
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
