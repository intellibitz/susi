use susi_error::EaiError;

fn main() -> susi_error::EaiResult<()> {
    let dir = susi_vendor_models::eco_profile::bundled_profiles_dir();
    for e in std::fs::read_dir(&dir).map_err(|e| EaiError::io(e.to_string()))? {
        let e = e.map_err(|e| EaiError::io(e.to_string()))?;
        let n = e.file_name();
        let t = std::fs::read_to_string(e.path()).map_err(|e| EaiError::io(e.to_string()))?;
        if let Err(e) = susi_vendor_models::eco_profile::parse(&t) {
            println!("{}: {e:?}", n.to_string_lossy());
        }
    }
    Ok(())
}
