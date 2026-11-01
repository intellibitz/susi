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
