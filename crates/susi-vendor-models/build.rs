use std::env;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

fn emit(dir: &Path, table: &str, output: &mut impl Write) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut paths = fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    writeln!(output, "pub(super) static {table}: &[(&str, &str)] = &[")?;
    for path in paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
    {
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| io::Error::other("non-UTF-8 ecosystem file name"))?;
        // JSON strings cannot contain literal line breaks. Keep all string
        // contents while omitting inter-line formatting and platform EOLs.
        let json: String = fs::read_to_string(&path)?.lines().map(str::trim).collect();
        writeln!(output, "({id:?}, {json:?}),")?;
    }
    writeln!(output, "];")
}

fn main() -> io::Result<()> {
    let root = Path::new("../../config/ecosystem");
    let dir = env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("missing OUT_DIR"))?;
    let mut output = BufWriter::new(fs::File::create(PathBuf::from(dir).join("ecosystem.rs"))?);
    emit(root, "ENTITIES", &mut output)?;
    emit(&root.join("profiles"), "PROFILES", &mut output)?;
    output.flush()
}
