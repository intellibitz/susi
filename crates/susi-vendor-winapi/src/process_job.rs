//! Spawn a suspended process and resume it only after assigning it to a
//! private kill-on-close job. All ordinary descendants inherit the job.

use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::ptr::{null, null_mut};
use winapi::um::{
    handleapi::INVALID_HANDLE_VALUE,
    jobapi2::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject},
    processthreadsapi::{OpenThread, ResumeThread},
    tlhelp32::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    },
    winbase::{CREATE_NO_WINDOW, CREATE_SUSPENDED},
    winnt::{
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, THREAD_SUSPEND_RESUME,
    },
};

/// Closing this non-inheritable job handle terminates its remaining members.
pub struct Job(OwnedHandle);

#[allow(unsafe_code)]
fn create() -> io::Result<Job> {
    // SAFETY: null selects default attributes and an unnamed, non-inheritable
    // job. OwnedHandle closes the unique valid handle, including error paths.
    let handle = unsafe { CreateJobObjectW(null_mut(), null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = Job(unsafe { OwnedHandle::from_raw_handle(handle.cast()) });
    // SAFETY: this Windows POD structure can be zero-initialized; the system
    // reads exactly its size and the job remains live during the call.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            (&raw mut info).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

#[allow(unsafe_code)]
fn attach_and_resume(job: &Job, child: &Child) -> io::Result<()> {
    // SAFETY: both live handles belong to these Rust owners. CREATE_SUSPENDED
    // prevents the child from running even one instruction before containment.
    if unsafe {
        AssignProcessToJobObject(job.0.as_raw_handle().cast(), child.as_raw_handle().cast())
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // Stable std closes the initial thread handle. Find the one thread of
    // the suspended process; its owned process handle keeps its PID valid.
    // SAFETY: successful snapshot/thread calls return owned Win32 handles.
    // The entry size matches THREADENTRY32, whose zero initialization is valid.
    unsafe {
        let raw = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = OwnedHandle::from_raw_handle(raw.cast());
        let mut entry: THREADENTRY32 = zeroed();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut found = Thread32First(snapshot.as_raw_handle().cast(), &mut entry);
        while found != 0 {
            if entry.th32OwnerProcessID == child.id() {
                let raw = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if raw.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let thread = OwnedHandle::from_raw_handle(raw.cast());
                return match ResumeThread(thread.as_raw_handle().cast()) {
                    1 => Ok(()),
                    u32::MAX => Err(io::Error::last_os_error()),
                    _ => Err(io::Error::other("unexpected workload thread suspend count")),
                };
            }
            found = Thread32Next(snapshot.as_raw_handle().cast(), &mut entry);
        }
    }
    Err(io::Error::other("suspended workload thread unavailable"))
}

/// Start the given command under a new private Job Object or fail closed.
///
/// # Errors
///
/// Returns an error if Windows cannot create, contain or resume the process.
pub fn spawn(command: &mut Command) -> io::Result<(Child, Job)> {
    let job = create()?;
    let mut child = command
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
        .spawn()?;
    if let Err(error) = attach_and_resume(&job, &child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok((child, job))
}
