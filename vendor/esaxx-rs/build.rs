#[cfg(feature = "cpp")]
#[cfg(not(target_os = "macos"))]
fn main() {
    // Upstream `.static_crt(true)` is dropped: on MSVC it emits /MT while the
    // rest of the tree (onnxruntime, zstd) links /MD, and mixing CRT models
    // fails with LNK2038. On non-MSVC toolchains the flag is a no-op anyway.
    cc::Build::new()
        .cpp(true)
        .flag("-std=c++11")
        .file("src/esaxx.cpp")
        .include("src")
        .compile("esaxx");
}

#[cfg(feature = "cpp")]
#[cfg(target_os = "macos")]
fn main() {
    cc::Build::new()
        .cpp(true)
        .flag("-std=c++11")
        .flag("-stdlib=libc++")
        .static_crt(true)
        .file("src/esaxx.cpp")
        .include("src")
        .compile("esaxx");
}

#[cfg(not(feature = "cpp"))]
fn main() {}
