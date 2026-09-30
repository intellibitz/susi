fn main() -> susi_error::EaiResult<()> {
    let kb = susi_vendor_models::eco_store::load_dir(
        &susi_vendor_models::eco_store::bundled_source_dir(),
    )?;
    for i in susi_vendor_models::eco_consistency::check(&kb) {
        println!("{}: {}", i.path, i.message);
    }
    Ok(())
}
