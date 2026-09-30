fn main() {
    let kb = susi_vendor_models::eco_store::load_dir(
        &susi_vendor_models::eco_store::bundled_source_dir(),
    )
    .unwrap();
    for i in susi_vendor_models::eco_consistency::check(&kb) {
        println!("{}: {}", i.path, i.message);
    }
}
