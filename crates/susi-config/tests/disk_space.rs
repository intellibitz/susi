use std::path::Path;
use susi_config::disk_space;

#[test]
#[cfg(unix)]
fn native_probe_returns_real_capacity_and_caller_availability(
) -> Result<(), Box<dyn std::error::Error>> {
    let (free, total) =
        disk_space::available(Path::new(".")).ok_or("native filesystem probe failed")?;
    assert!(total > 0);
    assert!(free <= total);
    Ok(())
}

#[test]
#[cfg(unix)]
fn unix_probe_does_not_substitute_a_parent_for_an_absent_path(
) -> Result<(), Box<dyn std::error::Error>> {
    let absent = std::env::temp_dir().join(format!(
        "susi-disk-probe-absent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    assert!(!absent.exists());
    assert_eq!(disk_space::available(&absent), None);
    Ok(())
}

#[test]
#[cfg(unix)]
fn unix_native_probe_preserves_rejection_of_non_utf8_and_nul_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    assert_eq!(
        disk_space::available(Path::new(OsStr::from_bytes(b"/\xff"))),
        None
    );
    assert_eq!(disk_space::available(Path::new("/\0")), None);
}
