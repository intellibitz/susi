fn main() {
    let dir = susi_vendor_models::eco_profile::bundled_profiles_dir();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let n = e.file_name();
        let t = std::fs::read_to_string(e.path()).unwrap();
        if let Err(e) = susi_vendor_models::eco_profile::parse(&t) {
            println!("{}: {e:?}", n.to_string_lossy());
        }
    }
}
