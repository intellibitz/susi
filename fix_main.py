import re

with open('src/main.rs', 'r') as f:
    code = f.read()

bad_code = """    let cfg = susi_engine::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let max_size = cfg.max_stdin_size_bytes;
    ;
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &timeout as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as u32,
            );
        }
    }
    let mut buffer = Vec::new();"""

good_code = """    let cfg = susi_engine::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let max_size = cfg.max_stdin_size_bytes;
    let mut buffer = Vec::new();"""

code = code.replace(bad_code, good_code)

with open('src/main.rs', 'w') as f:
    f.write(code)

