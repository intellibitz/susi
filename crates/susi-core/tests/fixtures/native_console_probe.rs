//! Test-only native visibility probe; compiled separately from unsafe-forbidden core.
#![deny(unsafe_code)]

// SAFETY: both OS functions have the documented Windows system ABI. The console
// HWND is borrowed, never dereferenced or freed, and is used only for the OS
// visibility query. This fixture is not linked into the shipped product.
#[allow(unsafe_code)]
fn console_is_visible() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleWindow() -> *mut std::ffi::c_void;
    }
    #[link(name = "user32")]
    extern "system" {
        fn IsWindowVisible(window: *mut std::ffi::c_void) -> i32;
    }
    // SAFETY: the OS supplies the HWND; a null handle means no console window.
    unsafe {
        let window = GetConsoleWindow();
        !window.is_null() && IsWindowVisible(window) != 0
    }
}

fn main() -> Result<std::process::ExitCode, std::io::Error> {
    use std::io::Read;
    if console_is_visible() {
        return Ok(std::process::ExitCode::from(21));
    }
    let mut input = Vec::new();
    if std::io::stdin().read_to_end(&mut input)? != 0 {
        return Ok(std::process::ExitCode::from(22));
    }
    println!("native-probe-output");
    eprintln!("native-probe-error");
    Ok(std::process::ExitCode::SUCCESS)
}
