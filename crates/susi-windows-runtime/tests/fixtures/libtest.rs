use susi_windows_runtime as _;

#[test]
fn native_libtest_retains_thread_and_unwind_semantics() {
    assert!(std::env::args_os().any(|argument| argument == "--exact"));
    assert_eq!(std::thread::spawn(|| 42).join().unwrap(), 42);
    assert!(std::panic::catch_unwind(|| panic!("caught fixture panic")).is_err());
}

#[test]
#[ignore = "qualification driver explicitly requests this expected failing case"]
fn native_libtest_failure_status_is_preserved() {
    panic!("intentional fixture failure");
}

#[test]
fn fixture_subtraction_is_exact() {
    assert_eq!(100_i32 - 58_i32, 42_i32);
}

#[test]
fn fixture_string_len_is_deterministic() {
    let label = "susi-windows-runtime";
    assert_eq!(label.len(), 20);
}
