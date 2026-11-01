fn main() {
    if std::env::args().nth(1).as_deref()==Some("panic") { panic!("abort policy preserved"); }
    let worker=std::thread::spawn(||(0_u64..10000).sum::<u64>());
    assert_eq!(worker.join().unwrap(),49_995_000);
    let mut bytes=vec![42_u8;65536];bytes.copy_within(0..32768,16384);assert_eq!(bytes.iter().map(|b|u64::from(*b)).sum::<u64>(),2_752_512);
    println!("PASS optimized abort runtime thread/memory functionality");
}
