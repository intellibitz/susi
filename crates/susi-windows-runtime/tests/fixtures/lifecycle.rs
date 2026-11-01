use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(susi_runtime_rlib)]
use susi_windows_runtime as _;
static INITIALIZED: AtomicUsize = AtomicUsize::new(0);
static ATTACHED: AtomicUsize = AtomicUsize::new(0);
static THREAD_ATTACHED: AtomicUsize = AtomicUsize::new(0);
static THREAD_DETACHED: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn initialize() {
    assert_eq!(INITIALIZED.load(Ordering::SeqCst), 21);
    INITIALIZED.store(42, Ordering::SeqCst);
}
unsafe extern "C" fn c_initialize() -> i32 {
    INITIALIZED.store(21, Ordering::SeqCst);
    i32::from(std::env::var_os("SUSI_PROBE_FAIL_INIT").is_some())
}
#[used]
#[link_section = ".CRT$XIU"]
static C_INITIALIZE: unsafe extern "C" fn() -> i32 = c_initialize;
#[used]
#[link_section = ".CRT$XCU"]
static INITIALIZE: unsafe extern "C" fn() = initialize;
unsafe extern "system" fn tls_callback(
    _: *mut std::ffi::c_void,
    reason: u32,
    _: *mut std::ffi::c_void,
) {
    match reason {
        1 => {
            ATTACHED.fetch_add(1, Ordering::SeqCst);
        }
        2 => {
            THREAD_ATTACHED.fetch_add(1, Ordering::SeqCst);
        }
        3 => {
            THREAD_DETACHED.fetch_add(1, Ordering::SeqCst);
        }
        _ => {}
    }
}
#[used]
#[link_section = ".CRT$XLB"]
static TLS_CALLBACK: unsafe extern "system" fn(*mut std::ffi::c_void, u32, *mut std::ffi::c_void) =
    tls_callback;
struct Cleanup(&'static str);
unsafe extern "C" fn preterminate() {
    drop(Cleanup("preterm"));
}
unsafe extern "C" fn terminate() {
    drop(Cleanup("term"));
}
unsafe extern "C" fn user_exit() {
    drop(Cleanup("user-exit"));
}
#[used]
#[link_section = ".CRT$XPU"]
static PRETERMINATE: unsafe extern "C" fn() = preterminate;
#[used]
#[link_section = ".CRT$XTU"]
static TERMINATE: unsafe extern "C" fn() = terminate;
impl Drop for Cleanup {
    fn drop(&mut self) {
        use std::io::Write;
        let path = std::env::var_os("SUSI_PROBE_LOG").unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{}", self.0).unwrap();
    }
}
#[repr(align(64))]
struct Aligned64(u8);
#[repr(align(4096))]
struct Aligned4096(u8);
thread_local! { static ALIGN64: Aligned64 = const { Aligned64(64) }; static ALIGN4096: Aligned4096 = const { Aligned4096(4) }; }
fn check_alignment() {
    ALIGN64.with(|v| {
        assert_eq!(v as *const _ as usize % 64, 0);
        assert_eq!(v.0, 64);
    });
    ALIGN4096.with(|v| {
        assert_eq!(v as *const _ as usize % 4096, 0);
        assert_eq!(v.0, 4);
    });
}
thread_local! { static LOCAL: Cleanup = const { Cleanup("tls") }; }
fn main() -> std::process::ExitCode {
    assert_eq!(INITIALIZED.load(Ordering::SeqCst), 42);
    assert_eq!(ATTACHED.load(Ordering::SeqCst), 1);
    assert_eq!(std::hint::black_box([7u8; 65536])[65535], 7);
    check_alignment();
    LOCAL.with(|_| {});
    let _cleanup = Cleanup("main-drop");
    let args: Vec<_> = std::env::args_os().collect();
    extern "C" {
        fn atexit(callback: unsafe extern "C" fn()) -> i32;
        #[link_name = "\x01??_7type_info@@6B@"]
        static DESCRIPTOR:
            [unsafe extern "C" fn(*mut std::ffi::c_void, u32) -> *mut std::ffi::c_void; 1];
    }
    assert_eq!(unsafe { atexit(user_exit) }, 0);
    println!("args={args:?}");
    assert_eq!(args[1], "unicode-λ space");
    assert_eq!(args[2], "");
    std::thread::spawn(|| {
        check_alignment();
        LOCAL.with(|_| {});
        assert_eq!(std::thread::current().name(), None);
    })
    .join()
    .unwrap();
    assert!(
        THREAD_ATTACHED.load(Ordering::SeqCst) > 0 && THREAD_DETACHED.load(Ordering::SeqCst) > 0
    );
    if args.get(3).is_some_and(|value| value == "exit") {
        std::process::exit(29);
    }
    for (mode, flags) in [("rtti0", 0), ("rtti1", 1)] {
        if args.get(3).is_some_and(|value| value == mode) {
            unsafe {
                DESCRIPTOR[0](std::ptr::null_mut(), flags);
            }
            panic!("foreign RTTI must fail closed");
        }
    }
    if args
        .get(3)
        .is_some_and(|value| value == "uncaught" || value == "abort")
    {
        panic!("qualification terminal panic");
    }
    #[cfg(panic = "unwind")]
    assert!(std::panic::catch_unwind(|| panic!("caught qualification panic")).is_err());
    std::process::ExitCode::from(37)
}
