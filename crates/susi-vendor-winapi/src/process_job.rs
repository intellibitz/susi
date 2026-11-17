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

impl Job {
    /// Create an unnamed, non-inheritable kill-on-close job.
    pub fn new() -> io::Result<Self> {
        create()
    }

    /// Borrow a live child handle while assigning it to this live job.
    #[allow(unsafe_code)]
    pub fn assign(&self, child: &Child) -> io::Result<()> {
        // SAFETY: both handles are borrowed from live Rust owners; neither is
        // closed or transferred by AssignProcessToJobObject.
        if unsafe {
            AssignProcessToJobObject(self.0.as_raw_handle().cast(), child.as_raw_handle().cast())
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// End the job's processes while retaining ownership of its handle.
    #[allow(unsafe_code)]
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: the borrowed handle remains valid for this entire call.
        if unsafe {
            winapi::um::jobapi2::TerminateJobObject(self.0.as_raw_handle().cast(), exit_code)
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[allow(unsafe_code)]
fn open_process(pid: u32, access: u32) -> io::Result<OwnedHandle> {
    // SAFETY: integer inputs only. A non-null return is a newly owned handle.
    let raw = unsafe { winapi::um::processthreadsapi::OpenProcess(access, 0, pid) };
    if raw.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
    }
}

/// Process creation time in 100 ns ticks, used to reject reused parent PIDs.
#[allow(unsafe_code)]
pub fn created_at(pid: u32) -> io::Result<u64> {
    use winapi::shared::minwindef::FILETIME;
    let process = open_process(pid, winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION)?;
    let blank = || FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut creation, mut exit, mut kernel, mut user) = (blank(), blank(), blank(), blank());
    // SAFETY: query handle is live; four distinct initialized FILETIME values
    // provide writable storage of the exact size required by the OS.
    if unsafe {
        winapi::um::processthreadsapi::GetProcessTimes(
            process.as_raw_handle().cast(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

/// Terminate one process by a newly opened, privately owned handle.
#[allow(unsafe_code)]
pub fn terminate_process(pid: u32, exit_code: u32) -> io::Result<()> {
    let process = open_process(pid, winapi::um::winnt::PROCESS_TERMINATE)?;
    // SAFETY: the handle stays open until the call returns.
    if unsafe {
        winapi::um::processthreadsapi::TerminateProcess(process.as_raw_handle().cast(), exit_code)
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

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
