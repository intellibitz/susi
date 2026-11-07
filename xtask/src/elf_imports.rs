//! Bounded ELF loader inventory. This establishes dynamic imports, not absence
//! of embedded native code; qualification must also inspect the linker map.
use std::collections::BTreeSet;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Inventory {
    pub(crate) machine: u16,
    pub(crate) wide: bool,
    pub(crate) little_endian: bool,
    pub(crate) interpreter: String,
    pub(crate) needed: Vec<String>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    little: bool,
}
impl Reader<'_> {
    fn range(&self, offset: u64, size: u64) -> Result<&[u8]> {
        let end = offset.checked_add(size).ok_or("ELF range overflow")?;
        let start = usize::try_from(offset).map_err(|_| "ELF offset overflow")?;
        let end = usize::try_from(end).map_err(|_| "ELF size overflow")?;
        self.bytes
            .get(start..end)
            .ok_or_else(|| "truncated ELF range".into())
    }
    fn number(&self, offset: u64, width: u64) -> Result<u64> {
        let bytes = self.range(offset, width)?;
        let mut value = 0u64;
        if self.little {
            for byte in bytes.iter().rev() {
                value = (value << 8) | u64::from(*byte);
            }
        } else {
            for byte in bytes {
                value = (value << 8) | u64::from(*byte);
            }
        }
        Ok(value)
    }
}

fn string(bytes: &[u8]) -> Result<String> {
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or("unterminated ELF string")?;
    if end == 0 || end > 4096 {
        return Err("invalid ELF string length".into());
    }
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| "nonUTF8 ELF string")?;
    if text.bytes().any(|b| !b.is_ascii_graphic()) {
        return Err("invalid ELF string bytes".into());
    }
    Ok(text.to_owned())
}

pub(crate) fn inspect(bytes: &[u8]) -> Result<Inventory> {
    if bytes.len() < 16 || &bytes[..4] != b"\x7fELF" || bytes[6] != 1 {
        return Err("invalid ELF identification".into());
    }
    let wide = match bytes[4] {
        1 => false,
        2 => true,
        _ => return Err("invalid ELF class".into()),
    };
    let reader = Reader {
        bytes,
        little: match bytes[5] {
            1 => true,
            2 => false,
            _ => return Err("invalid ELF encoding".into()),
        },
    };
    let header = if wide { 64 } else { 52 };
    reader.range(0, header)?;
    if !matches!(reader.number(16, 2)?, 2 | 3) || reader.number(20, 4)? != 1 {
        return Err("ELF must be an executable or shared image".into());
    }
    let machine = reader.number(18, 2)? as u16;
    let phoff = reader.number(if wide { 32 } else { 28 }, if wide { 8 } else { 4 })?;
    let phsize = reader.number(if wide { 54 } else { 42 }, 2)?;
    let count = reader.number(if wide { 56 } else { 44 }, 2)?;
    if count == 0 || count > 4096 || phsize != if wide { 56 } else { 32 } {
        return Err("invalid ELF program table".into());
    }
    reader.range(phoff, phsize * count)?;
    let mut loads = Vec::new();
    let mut dynamic = None;
    let mut interpreter = None;
    for i in 0..count {
        let at = phoff + i * phsize;
        let kind = reader.number(at, 4)?;
        let width = if wide { 8 } else { 4 };
        let offset = reader.number(at + if wide { 8 } else { 4 }, width)?;
        let address = reader.number(at + if wide { 16 } else { 8 }, width)?;
        let size = reader.number(at + if wide { 32 } else { 16 }, width)?;
        let memory_size = reader.number(at + if wide { 40 } else { 20 }, width)?;
        reader.range(offset, size)?;
        match kind {
            1 => {
                if size > memory_size {
                    return Err("ELF load exceeds memory range".into());
                }
                address.checked_add(size).ok_or("ELF address overflow")?;
                loads.push((address, offset, size));
            }
            2 => {
                if dynamic.replace((offset, size)).is_some() {
                    return Err("multiple ELF dynamic tables".into());
                }
            }
            3 if interpreter
                .replace(string(reader.range(offset, size)?)?)
                .is_some() =>
            {
                return Err("multiple ELF interpreters".into());
            }
            3 => {}
            _ => {}
        }
    }
    let interpreter = interpreter.ok_or("ELF has no dynamic interpreter")?;
    if !interpreter.starts_with('/') || interpreter.split('/').any(|p| p == "..") {
        return Err("ELF interpreter must be an absolute OS path".into());
    }
    let (offset, size) = dynamic.ok_or("ELF has no dynamic table")?;
    let width = if wide { 8 } else { 4 };
    if size % (width * 2) != 0 || size / (width * 2) > 65536 {
        return Err("invalid ELF dynamic table size".into());
    }
    let mut table = None;
    let mut table_size = None;
    let mut names = Vec::new();
    let mut terminated = false;
    for i in 0..size / (width * 2) {
        let at = offset + i * width * 2;
        let tag = reader.number(at, width)?;
        let value = reader.number(at + width, width)?;
        match tag {
            0 => {
                terminated = true;
                break;
            }
            1 => names.push(value),
            5 => {
                if table.replace(value).is_some() {
                    return Err("duplicate ELF string table".into());
                }
            }
            10 => {
                if table_size.replace(value).is_some() {
                    return Err("duplicate ELF string size".into());
                }
            }
            15 | 29 => return Err("ELF runtime search path is not admitted".into()),
            _ => {}
        }
    }
    if !terminated || names.is_empty() {
        return Err("ELF dynamic imports missing or unterminated".into());
    }
    let address = table.ok_or("ELF string table missing")?;
    let size = table_size.ok_or("ELF string table size missing")?;
    let mut ranges = loads.iter().filter_map(|(base, offset, length)| {
        let relative = address.checked_sub(*base)?;
        (relative.checked_add(size)? <= *length).then_some(offset + relative)
    });
    let offset = ranges.next().ok_or("ELF strings are not file-backed")?;
    if ranges.next().is_some() {
        return Err("ambiguous ELF string mapping".into());
    }
    let strings = reader.range(offset, size)?;
    let mut needed = BTreeSet::new();
    for name in names {
        let at = usize::try_from(name).map_err(|_| "ELF name offset overflow")?;
        let name = string(strings.get(at..).ok_or("ELF name outside string table")?)?;
        if name.contains('/') || name == "." || name == ".." {
            return Err("ELF import must be a soname".into());
        }
        needed.insert(name);
    }
    Ok(Inventory {
        machine,
        wide,
        little_endian: reader.little,
        interpreter,
        needed: needed.into_iter().collect(),
    })
}

/// Admission is contingent on controlled dynamic linkage and artifact proof.
/// The reviewed build script preprocesses headers; it does not compile a C
/// object in this exact feature configuration.
pub(crate) fn reviewed_openssl(
    name: &str,
    version: &str,
    target: &str,
    features: &[String],
) -> bool {
    matches!(
        target,
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
    ) && name == "openssl-sys"
        && version == "0.9.117"
        && features.is_empty()
}

pub(crate) fn validate_os_imports(
    inventory: &Inventory,
    target: &str,
    require_tls: bool,
) -> Result<()> {
    let (machine, loader) = match target {
        "x86_64-unknown-linux-gnu" => (62, "/lib64/ld-linux-x86-64.so.2"),
        "aarch64-unknown-linux-gnu" => (183, "/lib/ld-linux-aarch64.so.1"),
        _ => return Err("Linux ELF target has no reviewed OS boundary".into()),
    };
    if inventory.machine != machine
        || inventory.interpreter != loader
        || !inventory.wide
        || !inventory.little_endian
    {
        return Err("ELF machine/interpreter does not match selected native target".into());
    }
    for name in &inventory.needed {
        if Some(name.as_str()) == loader.rsplit('/').next() {
            continue;
        }
        if !matches!(
            name.as_str(),
            "libc.so.6"
                | "libm.so.6"
                | "libdl.so.2"
                | "libpthread.so.0"
                | "librt.so.1"
                | "libgcc_s.so.1"
                | "libssl.so.3"
                | "libcrypto.so.3"
        ) {
            return Err(format!("unreviewed ELF runtime dependency: {name}"));
        }
    }
    if require_tls
        && ["libssl.so.3", "libcrypto.so.3"]
            .iter()
            .any(|wanted| !inventory.needed.iter().any(|name| name == wanted))
    {
        return Err("artifact lacks required dynamic OpenSSL 3 imports".into());
    }
    Ok(())
}

/// LLD maps are emitted by the pinned native compiler. Inspect every object
/// input, not only DT_NEEDED: a dynamic dependency can coexist with static code.
/// Rust rlibs remain subject to the selected-graph and sysroot provenance gates.
pub(crate) fn validate_link_map(map: &str) -> Result<Vec<String>> {
    if !map.lines().next().is_some_and(|line| {
        line.split_whitespace().collect::<Vec<_>>()
            == ["VMA", "LMA", "Size", "Align", "Out", "In", "Symbol"]
    }) {
        return Err("unrecognized native LLD map".into());
    }
    let mut inputs = BTreeSet::new();
    for line in map.lines().skip(1) {
        for word in line.split_whitespace() {
            if word.ends_with(".a") || word.contains(".a(") || word.contains(".a:") {
                return Err(format!("foreign static archive in ELF link: {word}"));
            }
        }
        let mut input = line.trim_start();
        for _ in 0..4 {
            let Some(end) = input.find(char::is_whitespace) else {
                input = "";
                break;
            };
            input = input[end..].trim_start();
        }
        let Some((object, _)) = input.split_once(":(") else {
            continue;
        };
        if object == "<internal>" {
            continue;
        }
        if object.contains(".rlib(") && object.ends_with(')') {
            inputs.insert(object.to_owned());
            continue;
        }
        if object.ends_with(".rcgu.o") {
            inputs.insert(object.to_owned());
            continue;
        }
        let path = std::path::Path::new(object);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if matches!(
            name,
            "Scrt1.o" | "crt1.o" | "crti.o" | "crtn.o" | "crtbeginS.o" | "crtendS.o"
        ) {
            let canonical = path
                .canonicalize()
                .map_err(|_| "cannot resolve OS startup object")?;
            if canonical.starts_with("/usr/lib") || canonical.starts_with("/lib") {
                inputs.insert(canonical.to_string_lossy().into_owned());
                continue;
            }
        }
        return Err(format!("unreviewed ELF object input: {object}"));
    }
    // Thin LTO materializes archive members as compiler-generated rcgu objects.
    // The separately verified selected graph/sysroot remains their provenance.
    if !inputs.iter().any(|input| input.ends_with(".rcgu.o")) {
        return Err("ELF map lacks Rust archive/object provenance".into());
    }
    Ok(inputs.into_iter().collect())
}

/// Select system OpenSSL only. Target-prefixed variables take precedence in
/// openssl-sys, so remove every inherited selector before establishing policy.
pub(crate) fn configure_system_openssl(
    command: &mut std::process::Command,
    target: &str,
) -> Result<()> {
    super::compiler_env::configure_system_openssl(command, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        b[16..18].copy_from_slice(&3u16.to_le_bytes());
        b[18..20].copy_from_slice(&62u16.to_le_bytes());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&3u16.to_le_bytes());
        for (i, kind, offset, address, size) in [
            (0, 1u32, 0u64, 4096u64, 512u64),
            (1, 2, 256, 4352, 80),
            (2, 3, 336, 4432, 28),
        ] {
            let p = 64 + i * 56;
            b[p..p + 4].copy_from_slice(&kind.to_le_bytes());
            for (at, value) in [(8, offset), (16, address), (32, size), (40, size)] {
                b[p + at..p + at + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        for (i, tag, value) in [(0, 5u64, 4480u64), (1, 10, 40), (2, 1, 1), (3, 1, 13)] {
            let p = 256 + i * 16;
            b[p..p + 8].copy_from_slice(&tag.to_le_bytes());
            b[p + 8..p + 16].copy_from_slice(&value.to_le_bytes());
        }
        b[336..364].copy_from_slice(b"/lib64/ld-linux-x86-64.so.2\0");
        b[384..412].copy_from_slice(b"\0libssl.so.3\0libcrypto.so.3\0");
        b
    }
    fn encode(bytes: &mut [u8], at: usize, width: usize, value: u64, little: bool) {
        let encoded = if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        bytes[at..at + width].copy_from_slice(if little {
            &encoded[..width]
        } else {
            &encoded[8 - width..]
        });
    }
    fn portable_fixture(wide: bool, little: bool) -> Vec<u8> {
        let mut b = vec![0; 512];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = if wide { 2 } else { 1 };
        b[5] = if little { 1 } else { 2 };
        b[6] = 1;
        for (at, width, value) in [(16, 2, 3), (18, 2, 183), (20, 4, 1)] {
            encode(&mut b, at, width, value, little);
        }
        let width = if wide { 8 } else { 4 };
        let phsize = if wide { 56 } else { 32 };
        encode(&mut b, if wide { 32 } else { 28 }, width, 64, little);
        encode(&mut b, if wide { 54 } else { 42 }, 2, phsize as u64, little);
        encode(&mut b, if wide { 56 } else { 44 }, 2, 3, little);
        for (i, kind, offset, address, size) in [
            (0, 1, 0, 4096, 512),
            (1, 2, 256, 4352, 80),
            (2, 3, 336, 4432, 28),
        ] {
            let p = 64 + i * phsize;
            encode(&mut b, p, 4, kind, little);
            for (at, value) in [
                (if wide { 8 } else { 4 }, offset),
                (if wide { 16 } else { 8 }, address),
                (if wide { 32 } else { 16 }, size),
                (if wide { 40 } else { 20 }, size),
            ] {
                encode(&mut b, p + at, width, value, little);
            }
        }
        for (i, tag, value) in [(0, 5, 4480), (1, 10, 40), (2, 1, 1), (3, 1, 13)] {
            encode(&mut b, 256 + i * width * 2, width, tag, little);
            encode(&mut b, 256 + i * width * 2 + width, width, value, little);
        }
        b[336..364].copy_from_slice(b"/lib64/ld-linux-x86-64.so.2\0");
        b[384..412].copy_from_slice(b"\0libssl.so.3\0libcrypto.so.3\0");
        b
    }
    #[test]
    fn both_classes_and_byte_orders_use_virtual_address_mapping() {
        for wide in [false, true] {
            for little in [false, true] {
                let result = inspect(&portable_fixture(wide, little)).unwrap();
                assert_eq!(result.machine, 183);
                assert_eq!(result.needed, ["libcrypto.so.3", "libssl.so.3"]);
            }
        }
    }
    #[test]
    fn link_maps_reject_static_and_unknown_objects() {
        let good = "VMA LMA Size Align Out In Symbol\n0 0 0 1 /build/libstd.rlib(std.rcgu.o):(.text)\n0 0 0 1 /build/app.rcgu.o:(.text)\n";
        assert!(validate_link_map(good).is_ok());
        let thin_lto = "VMA LMA Size Align Out In Symbol\n0 0 0 1 /build/app.rcgu.o:(.text)\n0 0 0 1 /build/std.rcgu.o.rcgu.o:(.text)\n";
        assert!(validate_link_map(thin_lto).is_ok());
        for foreign in [
            "/lib/libssl.a(ssl.o)",
            "/lib/libcrypto.a:crypto.o",
            "/build/foreign.o",
            "/build/space directory/foreign.o",
        ] {
            assert!(validate_link_map(&format!("{good}0 0 0 1 {foreign}:(.text)\n")).is_err());
        }
        assert!(validate_link_map("not a map").is_err());
    }
    #[test]
    fn import_policy_requires_exact_platform_and_crypto() {
        let mut imports = inspect(&fixture()).unwrap();
        assert!(validate_os_imports(&imports, "x86_64-unknown-linux-gnu", true).is_ok());
        assert!(validate_os_imports(&imports, "aarch64-unknown-linux-gnu", true).is_err());
        imports.wide = false;
        assert!(validate_os_imports(&imports, "x86_64-unknown-linux-gnu", true).is_err());
        imports.wide = true;
        imports.little_endian = false;
        assert!(validate_os_imports(&imports, "x86_64-unknown-linux-gnu", true).is_err());
        imports.little_endian = true;
        imports.needed.push("ld-linux-x86-64.so.2".into());
        assert!(validate_os_imports(&imports, "x86_64-unknown-linux-gnu", true).is_ok());
        imports.needed.push("ld-linux-aarch64.so.1".into());
        assert!(validate_os_imports(&imports, "x86_64-unknown-linux-gnu", true).is_err());
        imports.needed.pop();
        // The product stdlib loads a qualified OS ICU at runtime; a direct ICU
        // link would pin one distribution's major and is no longer reviewed.
        for icu in [
            "libicuuc.so.76",
            "libicudata.so.76",
            "libicuuc.so.74",
            "libicuuc.so.72",
        ] {
            imports.needed.push(icu.into());
            assert!(validate_os_imports(&imports, "x86_64-unknown-linux-gnu", true).is_err());
            imports.needed.pop();
        }
        imports.needed.push("libssl.so.1.1".into());
        assert!(validate_os_imports(&imports, "x86_64-unknown-linux-gnu", true).is_err());
        imports.needed = vec!["libc.so.6".into()];
        assert!(validate_os_imports(&imports, "x86_64-unknown-linux-gnu", true).is_err());
    }
    #[test]
    fn native_binding_admission_is_exact() {
        assert!(reviewed_openssl(
            "openssl-sys",
            "0.9.117",
            "x86_64-unknown-linux-gnu",
            &[]
        ));
        for feature in ["vendored", "bindgen", "aws-lc", "boringssl"] {
            assert!(!reviewed_openssl(
                "openssl-sys",
                "0.9.117",
                "x86_64-unknown-linux-gnu",
                &[feature.into()]
            ));
        }
        assert!(!reviewed_openssl(
            "openssl-sys",
            "0.9.118",
            "x86_64-unknown-linux-gnu",
            &[]
        ));
    }
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn controlled_openssl_uses_only_system_dynamic_linkage() {
        let mut command = std::process::Command::new("cargo");
        command
            .env("X86_64_UNKNOWN_LINUX_GNU_OPENSSL_STATIC", "1")
            .env("LD_PRELOAD", "/tmp/foreign.so");
        configure_system_openssl(&mut command, "x86_64-unknown-linux-gnu").unwrap();
        for name in ["X86_64_UNKNOWN_LINUX_GNU_OPENSSL_STATIC", "LD_PRELOAD"] {
            assert!(command
                .get_envs()
                .any(|(key, value)| key == name && value.is_none()));
        }
        assert!(command.get_envs().any(
            |(key, value)| key == "OPENSSL_STATIC" && value == Some(std::ffi::OsStr::new("0"))
        ));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "OPENSSL_NO_VENDOR"
                && value == Some(std::ffi::OsStr::new("1"))));
    }
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn native_thin_lto_link_map_is_admitted() {
        let directory = std::env::temp_dir().join(format!(
            "susi thin lto {} {}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let source = directory.join("main.rs");
        let map = directory.join("main.map");
        let binary = directory.join("main");
        std::fs::write(&source, "fn main() { println!(\"native thin LTO\"); }\n").unwrap();
        let output = std::process::Command::new("rustc")
            .arg(&source)
            .args([
                "--edition",
                "2021",
                "-Copt-level=3",
                "-Clto=thin",
                "-Cpanic=abort",
            ])
            .arg(format!("-Clink-arg=-Wl,-Map={}", map.display()))
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        validate_link_map(&std::fs::read_to_string(&map).unwrap()).unwrap();
        validate_os_imports(
            &inspect(&std::fs::read(&binary).unwrap()).unwrap(),
            "x86_64-unknown-linux-gnu",
            false,
        )
        .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn records_dynamic_os_crypto_imports() {
        let result = inspect(&fixture()).unwrap();
        assert_eq!(result.machine, 62);
        assert_eq!(result.needed, ["libcrypto.so.3", "libssl.so.3"]);
    }
    #[test]
    fn malformed_images_fail_closed() {
        let bytes = fixture();
        for end in 0..bytes.len() {
            assert!(inspect(&bytes[..end]).is_err(), "length {end}");
        }
        for (at, value) in [
            (5, 0),
            (256, 29),
            (264, 255),
            (304, 29),
            (320, 1),
            (397, b'/'),
        ] {
            let mut bytes = fixture();
            bytes[at] = value;
            assert!(inspect(&bytes).is_err(), "offset {at}");
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn reads_actual_native_test_executable() {
        let bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let result = inspect(&bytes).unwrap();
        assert!(result.needed.iter().any(|name| name == "libc.so.6"));
    }
}
