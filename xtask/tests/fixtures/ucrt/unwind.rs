use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
struct DropCount(Arc<AtomicUsize>);
impl Drop for DropCount { fn drop(&mut self) { self.0.fetch_add(1,Ordering::SeqCst); } }
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let drops=Arc::new(AtomicUsize::new(0));
    let count=drops.clone();
    let error=std::panic::catch_unwind(move || {let _guard=DropCount(count); std::panic::panic_any(String::from("original payload"));});
    assert_eq!(error.unwrap_err().downcast_ref::<String>().unwrap(), "original payload");
    assert_eq!(drops.load(Ordering::SeqCst),1);
    let count=drops.clone();
    let outer=std::panic::catch_unwind(move || {let _outer=DropCount(count.clone());let inner=std::panic::catch_unwind(move || {let _inner=DropCount(count);panic!("inner");});assert!(inner.is_err());panic!("outer");});
    assert!(outer.is_err());assert_eq!(drops.load(Ordering::SeqCst),3);
    let count=drops.clone();
    let worker=std::thread::spawn(move || {let _guard=DropCount(count);panic!("worker");});assert!(worker.join().is_err());assert_eq!(drops.load(Ordering::SeqCst),4);
    let resumed=std::panic::catch_unwind(||{let payload=std::panic::catch_unwind(||panic!("resume")).unwrap_err();std::panic::resume_unwind(payload);});assert!(resumed.is_err());
    let mut bytes=vec![17_u8;16384];bytes.copy_within(0..8192,4096);assert!(bytes.iter().all(|b|*b==17));
    println!("PASS catch payload, destructor unwind, nested catches, thread panic, resume unwind, memory operations");
}
