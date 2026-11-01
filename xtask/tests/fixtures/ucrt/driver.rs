#[link(name="kernel32")]
unsafe extern "system" {fn SetErrorMode(mode:u32)->u32;}
fn main(){unsafe{SetErrorMode(0x0001|0x0002|0x8000);}
let argument=std::env::args_os().nth(1).unwrap(); let base=std::path::Path::new(&argument);
let good=std::process::Command::new(base.join("abort.exe")).output().unwrap();assert!(good.status.success());print!("{}",String::from_utf8_lossy(&good.stdout));
let abort=std::process::Command::new(base.join("abort.exe")).arg("panic").output().unwrap();assert!(!abort.status.success());println!("abort-panic exit={:?}",abort.status.code());
let missing=std::process::Command::new(base.join("missing.exe")).output().unwrap();assert_eq!(missing.status.code(),Some(0xc0000139_u32 as i32));assert!(missing.stdout.is_empty());println!("missing OS capability fails before main: {:?}",missing.status.code());}
