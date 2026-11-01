//! Safety boundary: loader-validated PE image, compiler-owned linker sections,
//! x64 C/system calling conventions, and explicitly qualified OS imports.
use core::{ffi::c_void, mem::size_of, ptr};

// SAFETY: these declarations match the qualified Windows x64 system ABI.
#[allow(unsafe_code)]
#[link(name = "kernel32")]
extern "system" {
    fn ExitProcess(code: u32) -> !;
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}
// SAFETY: declarations match the OS UCRT callback and cleanup C ABI.
#[allow(unsafe_code)]
#[link(name = "ucrt")]
extern "C" {
    fn _crt_atexit(callback: unsafe extern "C" fn()) -> i32;
    fn _cexit();
}
// SAFETY: pinned rustc emits main(int,char**); no private Rust ABI is called.
#[allow(unsafe_code)]
extern "C" {
    #[link_name = "main"]
    fn compiler_main(argc: i32, argv: *const *const u8) -> i32;
}

/// Forward Rust std's exit registration into the OS UCRT callback registry.
///
/// # Safety
/// The callback must have the C ABI and remain valid until process termination.
#[no_mangle]
// SAFETY: the function-local proof documents each qualified ABI operation.
#[allow(unsafe_code)]
pub unsafe extern "C" fn atexit(callback: unsafe extern "C" fn()) -> i32 {
    // SAFETY: C caller supplies a callback with the OS-required ABI/lifetime.
    unsafe { _crt_atexit(callback) }
}

// Presence marker used by compiler-generated floating-point object metadata.
// SAFETY: this unique compiler/PE ABI symbol is defined only by this runtime.
#[allow(unsafe_code)]
#[no_mangle]
pub static _fltused: i32 = 0;
// The Windows loader writes the allocated process TLS slot into this DWORD.
// SAFETY: this unique compiler/PE ABI symbol is defined only by this runtime.
#[allow(unsafe_code)]
#[no_mangle]
pub static mut _tls_index: u32 = 0;
// Writable sections are essential: read-only markers split the TLS image away
// from compiler-emitted mutable thread locals and invalidate their offsets.
// SAFETY: the qualified PE linker owns this exact metadata section layout.
#[allow(unsafe_code)]
#[used]
#[link_section = ".tls"]
static mut TLS_BEGIN: u8 = 0;
// SAFETY: the qualified PE linker owns this exact metadata section layout.
#[allow(unsafe_code)]
#[used]
#[link_section = ".tls$ZZZ"]
static mut TLS_END: u8 = 0;

type TlsCallback = Option<unsafe extern "system" fn(*mut c_void, u32, *mut c_void)>;
// SAFETY: the qualified PE linker owns this exact metadata section layout.
#[allow(unsafe_code)]
#[used]
#[link_section = ".CRT$XLA"]
static CALLBACK_BEGIN: TlsCallback = None;
// SAFETY: the qualified PE linker owns this exact metadata section layout.
#[allow(unsafe_code)]
#[used]
#[link_section = ".CRT$XLZ"]
static CALLBACK_END: TlsCallback = None;

#[repr(C)]
pub struct TlsDirectory {
    begin: *const u8,
    end: *const u8,
    index: *mut u32,
    callbacks: *const TlsCallback,
    zero_fill: u32,
    characteristics: u32,
}
// SAFETY: pointers describe immutable loader metadata. Only the loader owns
// mutation of the separately declared _tls_index before application entry.
#[allow(unsafe_code)]
unsafe impl Sync for TlsDirectory {}
#[used]
#[no_mangle]
#[link_section = ".rdata$T"]
// SAFETY: the unique PE TLS directory has the exact x64 loader structure and
// points only to this executable's compiler-owned TLS/callback/index storage.
#[allow(unsafe_code)]
pub static _tls_used: TlsDirectory = TlsDirectory {
    begin: ptr::addr_of!(TLS_BEGIN),
    end: ptr::addr_of!(TLS_END),
    index: ptr::addr_of_mut!(_tls_index),
    callbacks: (&CALLBACK_BEGIN as *const TlsCallback).wrapping_add(1),
    zero_fill: 0,
    // MSVC LINK fills the PE TLS alignment from the actual merged .tls section.
    // Qualification verifies align(64) and align(4096) locals on main+worker.
    characteristics: 0,
};

type Initializer = Option<unsafe extern "C" fn()>;
type FallibleInitializer = Option<unsafe extern "C" fn() -> i32>;
macro_rules! marker {
    ($name:ident, $section:literal, $ty:ty) => {
        // SAFETY: this null sentinel bounds one qualified PE callback range.
        #[allow(unsafe_code)]
        #[used]
        #[link_section = $section]
        static $name: $ty = None;
    };
}
marker!(C_BEGIN, ".CRT$XIA", FallibleInitializer);
marker!(C_END, ".CRT$XIZ", FallibleInitializer);
marker!(INIT_BEGIN, ".CRT$XCA", Initializer);
marker!(INIT_END, ".CRT$XCZ", Initializer);
marker!(PRETERM_BEGIN, ".CRT$XPA", Initializer);
marker!(PRETERM_END, ".CRT$XPZ", Initializer);
marker!(TERM_BEGIN, ".CRT$XTA", Initializer);
marker!(TERM_END, ".CRT$XTZ", Initializer);

// SAFETY: the function-local proof documents each qualified ABI operation.
#[allow(unsafe_code)]
fn boundary_failure() -> ! {
    // SAFETY: process termination is valid even before Rust runtime setup.
    unsafe { ExitProcess(255) }
}

// SAFETY: the function-local proof documents each qualified ABI operation.
#[allow(unsafe_code)]
fn image() -> (usize, usize) {
    // SAFETY: Windows returns this process's loader-validated PE image base.
    let base = unsafe { GetModuleHandleW(ptr::null()) } as usize;
    if base == 0 {
        boundary_failure();
    }
    // SAFETY: the Windows loader has mapped and validated these PE headers.
    let offset =
        unsafe { ptr::with_exposed_provenance::<u32>(base + 0x3c).read_unaligned() } as usize;
    if !(0x40..=0x100000).contains(&offset) {
        boundary_failure();
    }
    let Some(pe) = base.checked_add(offset) else {
        boundary_failure();
    };
    // SAFETY: e_lfanew names the already loader-validated NT headers.
    let (signature, magic, size) = unsafe {
        (
            ptr::with_exposed_provenance::<u32>(pe).read_unaligned(),
            ptr::with_exposed_provenance::<u16>(pe + 24).read_unaligned(),
            ptr::with_exposed_provenance::<u32>(pe + 24 + 56).read_unaligned(),
        )
    };
    if signature != 0x4550 || magic != 0x20b || size < 4096 {
        boundary_failure();
    }
    (base, size as usize)
}

// SAFETY: the function-local proof documents each qualified ABI operation.
#[allow(unsafe_code)]
fn visit(start: usize, end: usize, fallible: bool) -> bool {
    let (base, size) = image();
    let Ok(count) = super::table_slots(base, size, start, end) else {
        boundary_failure();
    };
    for index in 1..count {
        let Some(address) = index
            .checked_mul(size_of::<usize>())
            .and_then(|offset| start.checked_add(offset))
        else {
            boundary_failure();
        };
        // SAFETY: the bounded, aligned loader section contains compiler-emitted
        // pointer slots with the declared ABI. Integer address arithmetic avoids
        // inventing one Rust allocation spanning independently emitted statics.
        if fallible {
            let callback =
                unsafe { ptr::with_exposed_provenance::<FallibleInitializer>(address).read() };
            if let Some(call) = callback {
                // SAFETY: compiler-owned .CRT$XI callback ABI returns int.
                if unsafe { call() } != 0 {
                    return false;
                }
            }
        } else {
            let callback = unsafe { ptr::with_exposed_provenance::<Initializer>(address).read() };
            if let Some(call) = callback {
                // SAFETY: compiler-owned .CRT$XC/XP/XT callback ABI returns void.
                unsafe {
                    call();
                }
            }
        }
    }
    true
}

extern "C" fn terminate_tables() {
    visit(
        ptr::addr_of!(PRETERM_BEGIN) as usize,
        ptr::addr_of!(PRETERM_END) as usize,
        false,
    );
    visit(
        ptr::addr_of!(TERM_BEGIN) as usize,
        ptr::addr_of!(TERM_END) as usize,
        false,
    );
}

// Rust's MSVC panic descriptors require a type-info vtable pointer, but matching
// is by typename strings. They are static descriptors, not general C++ objects.
// No foreign RTTI/delete capability is claimed. Reject such calls explicitly.
extern "C" fn static_descriptor_destructor(_pointer: *mut c_void, _flags: u32) -> *mut c_void {
    boundary_failure()
}
// SAFETY: Rust EH requires this identity; all virtual calls fail closed.
#[allow(unsafe_code)]
#[export_name = "??_7type_info@@6B@"]
pub static DESCRIPTOR_VTABLE: [unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void; 1] =
    [static_descriptor_destructor];

/// PE entry point; invokes the compiler-generated C main, never private Rust APIs.
///
/// # Safety
/// Called exactly once by the Windows loader after PE import/TLS initialization.
#[no_mangle]
// SAFETY: the function-local proof documents each qualified ABI operation.
#[allow(unsafe_code)]
pub unsafe extern "system" fn susi_pe_entry() -> ! {
    // Register first: UCRT runs callbacks LIFO, so executable terminators run
    // after callbacks registered by constructors and Rust TLS initialization.
    // SAFETY: callback remains in the executable for the entire process lifetime.
    if unsafe { _crt_atexit(terminate_tables) } != 0 {
        boundary_failure();
    }
    if !visit(
        ptr::addr_of!(C_BEGIN) as usize,
        ptr::addr_of!(C_END) as usize,
        true,
    ) {
        boundary_failure();
    }
    visit(
        ptr::addr_of!(INIT_BEGIN) as usize,
        ptr::addr_of!(INIT_END) as usize,
        false,
    );
    // SAFETY: pinned x64 rustc emits C main(int,char**). Windows std ignores
    // these inputs and obtains arguments from GetCommandLineW. This single call
    // retains Rust lang_start, panic processing, std initialization and cleanup.
    let code = unsafe { compiler_main(0, ptr::null()) };
    // SAFETY: complete registered OS exit callbacks (including Rust FLS cleanup)
    // before terminating. ExitProcess alone would skip them.
    unsafe {
        _cexit();
        ExitProcess(code as u32)
    }
}
