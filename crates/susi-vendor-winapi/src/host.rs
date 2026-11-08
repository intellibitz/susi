//! Host readings on Windows through Win32, owned by the facade so every
//! consumer stays free of `unsafe`: physical and page-file memory, system CPU
//! times, uptime, the computer name, fixed drives, and the process table with
//! each process's image path, command line and environment block.
//!
//! Replaces the `sysinfo` facade (2026-11-07, lean-core): `sysinfo` pulled the
//! 85-second `windows` crate onto the critical path of every core build.
//! Every function answers `None` or an empty list on failure; nothing here
//! panics, and no raw pointer crosses out of this module.
//!
//! Only x86_64 is supported (AGENTS.md): the two PEB offsets below are the
//! 64-bit layout documented for `PEB.ProcessParameters` and
//! `RTL_USER_PROCESS_PARAMETERS.{CommandLine,Environment,EnvironmentSize}`.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use winapi::shared::minwindef::{DWORD, FALSE, FILETIME, LPCVOID, MAX_PATH};
use winapi::shared::ntdef::{HANDLE, LPWSTR, NTSTATUS, PVOID, ULONG};
use winapi::um::fileapi::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::memoryapi::ReadProcessMemory;
use winapi::um::processthreadsapi::{GetSystemTimes, OpenProcess};
use winapi::um::shellapi::CommandLineToArgvW;
use winapi::um::sysinfoapi::{
    ComputerNameDnsHostname, GetComputerNameExW, GetTickCount64, GlobalMemoryStatusEx,
    MEMORYSTATUSEX,
};
use winapi::um::tlhelp32::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use winapi::um::winbase::{LocalFree, QueryFullProcessImageNameW, DRIVE_FIXED};
use winapi::um::winnt::{PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ};

/// Physical memory and page file, in bytes, from `GlobalMemoryStatusEx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryStatus {
    /// Installed physical memory.
    pub total_physical: u64,
    /// Physical memory available to new work.
    pub available_physical: u64,
    /// Committed memory limit (physical plus page files).
    pub total_page_file: u64,
    /// Uncommitted part of that limit.
    pub available_page_file: u64,
}

/// `GlobalMemoryStatusEx`, or `None` when Windows refuses the call.
#[allow(unsafe_code)]
pub fn memory_status() -> Option<MemoryStatus> {
    let mut status = MEMORYSTATUSEX {
        dwLength: 0,
        dwMemoryLoad: 0,
        ullTotalPhys: 0,
        ullAvailPhys: 0,
        ullTotalPageFile: 0,
        ullAvailPageFile: 0,
        ullTotalVirtual: 0,
        ullAvailVirtual: 0,
        ullAvailExtendedVirtual: 0,
    };
    status.dwLength = DWORD::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).ok()?;
    // SAFETY: `status` is a writable MEMORYSTATUSEX whose dwLength names its
    // exact size, and it outlives the call.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) } != 0;
    ok.then_some(MemoryStatus {
        total_physical: status.ullTotalPhys,
        available_physical: status.ullAvailPhys,
        total_page_file: status.ullTotalPageFile,
        available_page_file: status.ullAvailPageFile,
    })
}

/// System-wide CPU times since boot, summed over every logical CPU, in
/// 100-nanosecond units. `kernel` includes `idle`, as Windows reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    /// Time every CPU spent idle.
    pub idle: u64,
    /// Time in kernel mode (idle included).
    pub kernel: u64,
    /// Time in user mode.
    pub user: u64,
}

fn filetime_u64(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// `GetSystemTimes`, or `None` when Windows refuses the call.
#[allow(unsafe_code)]
pub fn cpu_times() -> Option<CpuTimes> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    // SAFETY: three writable FILETIME values that outlive the call.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0;
    ok.then_some(CpuTimes {
        idle: filetime_u64(idle),
        kernel: filetime_u64(kernel),
        user: filetime_u64(user),
    })
}

/// Milliseconds since boot (`GetTickCount64`, never wraps).
#[allow(unsafe_code)]
pub fn uptime_ms() -> u64 {
    // SAFETY: takes no arguments and only reads the system tick counter.
    unsafe { GetTickCount64() }
}

/// This computer's DNS host name, or `None` when Windows reports none.
#[allow(unsafe_code)]
pub fn computer_name() -> Option<String> {
    let mut buffer = [0u16; 256];
    let mut size = DWORD::try_from(buffer.len()).ok()?;
    // SAFETY: `buffer` holds `size` UTF-16 units and both outlive the call;
    // Windows writes at most `size` units and updates `size` to the length.
    let ok =
        unsafe { GetComputerNameExW(ComputerNameDnsHostname, buffer.as_mut_ptr(), &mut size) } != 0;
    let len = usize::try_from(size).ok()?;
    if !ok || len == 0 || len > buffer.len() {
        return None;
    }
    let name = String::from_utf16_lossy(&buffer[..len]);
    (!name.trim().is_empty()).then_some(name)
}

/// One fixed (non-removable, non-network) drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedDrive {
    /// Root path, e.g. `C:\`.
    pub root: PathBuf,
    /// File system name as Windows reports it (`NTFS`, `ReFS`).
    pub file_system: String,
    /// Size in bytes.
    pub total_bytes: u64,
    /// Bytes available to the calling user.
    pub available_bytes: u64,
}

fn wide_nul(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Every fixed drive with its size and free space.
#[allow(unsafe_code)]
pub fn fixed_drives() -> Vec<FixedDrive> {
    // SAFETY: takes no arguments; returns a bit mask of drive letters.
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for bit in 0..26u32 {
        if mask & (1 << bit) == 0 {
            continue;
        }
        let letter = char::from(b'A' + bit as u8);
        let root = format!("{letter}:\\");
        let root_w = wide_nul(&root);
        // SAFETY: `root_w` is a NUL-terminated UTF-16 path that outlives the call.
        if unsafe { GetDriveTypeW(root_w.as_ptr()) } != DRIVE_FIXED {
            continue;
        }
        let mut available: u64 = 0;
        let mut total: u64 = 0;
        let mut free: u64 = 0;
        // SAFETY: the path is NUL-terminated; the three u64 outputs are
        // writable ULARGE_INTEGER-sized values that outlive the call.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                root_w.as_ptr(),
                (&mut available as *mut u64).cast(),
                (&mut total as *mut u64).cast(),
                (&mut free as *mut u64).cast(),
            )
        } != 0;
        if !ok {
            continue;
        }
        let mut fs_name = [0u16; 64];
        let fs_len = DWORD::try_from(fs_name.len()).unwrap_or(64);
        // SAFETY: the path is NUL-terminated; `fs_name` holds `fs_len` units
        // and outlives the call; every optional output is a null pointer,
        // which the API documents as "not wanted".
        let fs_ok = unsafe {
            GetVolumeInformationW(
                root_w.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_len,
            )
        } != 0;
        let file_system = if fs_ok {
            let end = fs_name
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(fs_name.len());
            String::from_utf16_lossy(&fs_name[..end])
        } else {
            String::new()
        };
        drives.push(FixedDrive {
            root: PathBuf::from(root),
            file_system,
            total_bytes: total,
            available_bytes: available,
        });
    }
    drives
}

/// One row of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    /// Process id.
    pub pid: u32,
    /// Parent process id as recorded at creation (the parent may be gone).
    pub parent_pid: u32,
    /// Image file name without a path (`susi.exe`).
    pub name: String,
}

/// A handle closed on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid handle owned by this value, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Every process in the system (`CreateToolhelp32Snapshot`).
#[allow(unsafe_code)]
pub fn processes() -> Vec<ProcessEntry> {
    // SAFETY: TH32CS_SNAPPROCESS with pid 0 snapshots every process; the
    // handle is owned below and closed on drop.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return Vec::new();
    }
    let snapshot = Handle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: 0,
        cntUsage: 0,
        th32ProcessID: 0,
        th32DefaultHeapID: 0,
        th32ModuleID: 0,
        cntThreads: 0,
        th32ParentProcessID: 0,
        pcPriClassBase: 0,
        dwFlags: 0,
        szExeFile: [0; MAX_PATH],
    };
    let Ok(size) = DWORD::try_from(std::mem::size_of::<PROCESSENTRY32W>()) else {
        return Vec::new();
    };
    entry.dwSize = size;
    let mut rows = Vec::new();
    // SAFETY: `snapshot.0` is a live snapshot handle; `entry` is a writable
    // PROCESSENTRY32W whose dwSize names its exact size and outlives the loop.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while more {
        let end = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        rows.push(ProcessEntry {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            name: String::from_utf16_lossy(&entry.szExeFile[..end]),
        });
        // SAFETY: as for Process32FirstW.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    rows
}

/// Open `pid` with `access`, or `None` when Windows refuses.
#[allow(unsafe_code)]
fn open(pid: u32, access: DWORD) -> Option<Handle> {
    // SAFETY: integer arguments only; a non-null result is a new handle this
    // value owns.
    let handle = unsafe { OpenProcess(access, FALSE, pid) };
    (!handle.is_null()).then_some(Handle(handle))
}

/// The full path of the image `pid` runs (`QueryFullProcessImageNameW`), or
/// `None` when the process is gone or not ours to inspect.
#[allow(unsafe_code)]
pub fn process_image_path(pid: u32) -> Option<PathBuf> {
    let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buffer = vec![0u16; 32 * 1024];
    let mut size = DWORD::try_from(buffer.len()).ok()?;
    // SAFETY: `process.0` is a live handle with query access; `buffer` holds
    // `size` units and both outlive the call; Windows updates `size` to the
    // written length.
    let ok =
        unsafe { QueryFullProcessImageNameW(process.0, 0, buffer.as_mut_ptr(), &mut size) } != 0;
    let len = usize::try_from(size).ok()?;
    if !ok || len == 0 || len > buffer.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buffer[..len])))
}

/// `PROCESS_BASIC_INFORMATION` as `ntdll` fills it (64-bit layout).
#[repr(C)]
struct ProcessBasicInformation {
    exit_status: NTSTATUS,
    peb_base_address: PVOID,
    affinity_mask: usize,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
}

#[link(name = "ntdll")]
#[allow(unsafe_code)]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        information_class: ULONG,
        information: PVOID,
        length: ULONG,
        return_length: *mut ULONG,
    ) -> NTSTATUS;
}

/// 64-bit `PEB.ProcessParameters` offset.
const PEB_PROCESS_PARAMETERS: usize = 0x20;
/// 64-bit `RTL_USER_PROCESS_PARAMETERS.CommandLine` (a UNICODE_STRING).
const PARAMS_COMMAND_LINE: usize = 0x70;
/// 64-bit `RTL_USER_PROCESS_PARAMETERS.Environment` (a pointer).
const PARAMS_ENVIRONMENT: usize = 0x80;
/// 64-bit `RTL_USER_PROCESS_PARAMETERS.EnvironmentSize` (bytes).
const PARAMS_ENVIRONMENT_SIZE: usize = 0x3F0;

/// Read `len` bytes at `address` in `process`, or `None` short of that.
#[allow(unsafe_code)]
fn read_memory(process: &Handle, address: usize, len: usize) -> Option<Vec<u8>> {
    if address == 0 || len == 0 {
        return None;
    }
    let mut buffer = vec![0u8; len];
    let mut read: usize = 0;
    // SAFETY: `process.0` has PROCESS_VM_READ; `buffer` holds `len` writable
    // bytes and `read` is a writable usize; both outlive the call. A bad
    // address makes the call fail rather than fault.
    let ok = unsafe {
        ReadProcessMemory(
            process.0,
            address as LPCVOID,
            buffer.as_mut_ptr().cast(),
            len,
            &mut read,
        )
    } != 0;
    (ok && read == len).then_some(buffer)
}

fn read_usize(process: &Handle, address: usize) -> Option<usize> {
    let bytes = read_memory(process, address, 8)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes);
    usize::try_from(u64::from_le_bytes(raw)).ok()
}

/// The address of `pid`'s `RTL_USER_PROCESS_PARAMETERS` block.
#[allow(unsafe_code)]
fn process_parameters(process: &Handle) -> Option<usize> {
    let mut info = ProcessBasicInformation {
        exit_status: 0,
        peb_base_address: std::ptr::null_mut(),
        affinity_mask: 0,
        base_priority: 0,
        unique_process_id: 0,
        inherited_from_unique_process_id: 0,
    };
    let len = ULONG::try_from(std::mem::size_of::<ProcessBasicInformation>()).ok()?;
    let mut returned: ULONG = 0;
    // SAFETY: class 0 is ProcessBasicInformation; `info` is a writable
    // struct of exactly `len` bytes; `returned` is a writable ULONG; all
    // outlive the call. ntdll is linked on every Windows host.
    let status = unsafe {
        NtQueryInformationProcess(
            process.0,
            0,
            (&mut info as *mut ProcessBasicInformation).cast(),
            len,
            &mut returned,
        )
    };
    if status != 0 || info.peb_base_address.is_null() {
        return None;
    }
    read_usize(
        process,
        info.peb_base_address as usize + PEB_PROCESS_PARAMETERS,
    )
}

/// The command line of `pid` split into arguments the way the C runtime
/// does (`CommandLineToArgvW`), or `None` when the process is gone, another
/// user's, or has no readable parameters block.
#[allow(unsafe_code)]
pub fn process_command_line(pid: u32) -> Option<Vec<String>> {
    let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
    let params = process_parameters(&process)?;
    // UNICODE_STRING: u16 Length (bytes), u16 MaximumLength, pad, PWSTR Buffer.
    let header = read_memory(&process, params + PARAMS_COMMAND_LINE, 16)?;
    let length = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let mut buffer_raw = [0u8; 8];
    buffer_raw.copy_from_slice(&header[8..16]);
    let buffer = usize::try_from(u64::from_le_bytes(buffer_raw)).ok()?;
    if length == 0 {
        return None;
    }
    let bytes = read_memory(&process, buffer, length)?;
    let mut wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    wide.push(0);
    let mut argc: i32 = 0;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `argc` is a
    // writable int. The returned array is freed exactly once with LocalFree.
    let argv = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
    if argv.is_null() {
        return None;
    }
    let count = usize::try_from(argc).unwrap_or(0);
    let mut args = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: `argv` holds `argc` valid NUL-terminated LPWSTR entries.
        let arg: LPWSTR = unsafe { *argv.add(index) };
        if arg.is_null() {
            continue;
        }
        let mut len = 0usize;
        // SAFETY: each entry is NUL-terminated; we read up to and not past it.
        while unsafe { *arg.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` units starting at `arg` are initialized UTF-16.
        let slice = unsafe { std::slice::from_raw_parts(arg, len) };
        args.push(String::from_utf16_lossy(slice));
    }
    // SAFETY: `argv` came from CommandLineToArgvW and is freed once, here.
    unsafe { LocalFree(argv.cast()) };
    (!args.is_empty()).then_some(args)
}

/// The environment block of `pid` as `KEY=VALUE` strings, or `None` when
/// the process is gone, another user's, or has no readable block.
pub fn process_environment(pid: u32) -> Option<Vec<String>> {
    let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
    let params = process_parameters(&process)?;
    let environment = read_usize(&process, params + PARAMS_ENVIRONMENT)?;
    let size = read_usize(&process, params + PARAMS_ENVIRONMENT_SIZE)?;
    // Sanity: a real block is small (well under 1 MiB).
    if size == 0 || size > 4 * 1024 * 1024 {
        return None;
    }
    let bytes = read_memory(&process, environment, size)?;
    let wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let entries: Vec<String> = wide
        .split(|c| *c == 0)
        .filter(|entry| !entry.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    (!entries.is_empty()).then_some(entries)
}

#[cfg(test)]
// Test-only assertions: a `None` here is the failure the test exists to report.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn memory_and_cpu_and_uptime_read() {
        let memory = memory_status().unwrap();
        assert!(memory.total_physical > 0);
        assert!(memory.available_physical <= memory.total_physical);
        let times = cpu_times().unwrap();
        assert!(times.kernel >= times.idle);
        assert!(uptime_ms() > 0);
        assert!(computer_name().is_some());
    }

    #[test]
    fn a_fixed_drive_exists_with_space() {
        let drives = fixed_drives();
        assert!(!drives.is_empty());
        assert!(drives.iter().any(|d| d.total_bytes > 0));
    }

    #[test]
    fn this_process_is_in_the_table_with_its_identity() {
        let me = std::process::id();
        let row = processes().into_iter().find(|p| p.pid == me).unwrap();
        assert!(row.name.to_ascii_lowercase().ends_with(".exe"));
        let exe = process_image_path(me).unwrap();
        assert_eq!(
            exe.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase()),
            Some(row.name.to_ascii_lowercase())
        );
        let args = process_command_line(me).unwrap();
        assert!(!args.is_empty(), "{args:?}");
        let env = process_environment(me).unwrap();
        assert!(env
            .iter()
            .any(|kv| kv.to_ascii_uppercase().starts_with("PATH=")));
    }
}
