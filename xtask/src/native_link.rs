//! Bind the PE entry point to an original Rust symbol and retain the linker
//! member inventory. OS import libraries are metadata, CRT implementation
//! members remain forbidden even when their DLL imports appear OS-only.
use std::{io, path::Path};
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
pub fn entry_address(bytes: &[u8]) -> io::Result<u64> {
    let word = |offset: usize| -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            bytes
                .get(offset..offset + 4)
                .ok_or_else(|| invalid("truncated PE entry header"))?
                .try_into()
                .map_err(|_| invalid("invalid PE entry word"))?,
        ))
    };
    if bytes.get(..2) != Some(b"MZ") {
        return Err(invalid("missing DOS header"));
    }
    let pe = word(0x3c)? as usize;
    if pe > bytes.len().saturating_sub(64) || bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(invalid("invalid PE header"));
    }
    // The entry evidence names a native executable: its COFF Machine must be
    // this host's architecture (AMD64 on x64, ARM64 on aarch64).
    crate::pe_imports::require_native_machine(bytes).map_err(invalid)?;
    let optional = pe + 24;
    if bytes.get(optional..optional + 2) != Some(&[0x0b, 0x02]) {
        return Err(invalid("native Rust entry requires PE32+"));
    }
    let base = u64::from_le_bytes(
        bytes
            .get(optional + 24..optional + 32)
            .ok_or_else(|| invalid("missing PE image base"))?
            .try_into()
            .map_err(|_| invalid("invalid PE image base"))?,
    );
    base.checked_add(u64::from(word(optional + 16)?))
        .ok_or_else(|| invalid("PE entry address overflow"))
}
pub fn validate_map(entry: u64, map: &str) -> io::Result<String> {
    validate_reader(
        entry,
        std::io::Cursor::new(map.as_bytes()),
        64 * 1024 * 1024,
    )
    .map(|value| value.0)
}

/// Streaming diagnostics admit the measured 231MB debug map without retaining
/// it or manufacturing a production receipt. Each individual line is bounded.
pub fn validate_reader(
    entry: u64,
    mut reader: impl io::BufRead,
    limit: u64,
) -> io::Result<(String, String)> {
    use io::{BufRead, Read};
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut line = String::new();
    let mut entry_symbol = None;
    let mut entry_marker = None;
    let mut origins = 0usize;
    loop {
        line.clear();
        let count = reader.by_ref().take(1024 * 1024 + 1).read_line(&mut line)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if count > 1024 * 1024 || total > limit {
            return Err(invalid("link map exceeds evidence budget"));
        }
        hash.update(line.as_bytes());
        let lower = line.to_ascii_lowercase();
        for forbidden in [
            "msvcrt.lib",
            "msvcrtd.lib",
            "vcruntime.lib",
            "vcruntimed.lib",
            "libcmt.lib",
            "libcmtd.lib",
            "libucrt.lib",
            "libucrtd.lib",
            "libvcruntime.lib",
            "exe_main.obj",
            "crtexe.obj",
            "maincrtstartup",
            "__scrt_common_main",
            "__scrt_initialize_crt",
        ] {
            if lower.contains(forbidden) {
                return Err(invalid(format!(
                    "foreign CRT implementation in link map: {forbidden}"
                )));
            }
        }
        if let Some(marker) = line.trim().strip_prefix("entry point at") {
            if entry_marker.replace(marker.trim().to_owned()).is_some() {
                return Err(invalid("duplicate link map entry marker"));
            }
        }
        let mut fields = line.split_whitespace();
        let (Some(section), Some(symbol), Some(address)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let origin = fields.next_back().unwrap_or_default();
        if symbol == "susi_pe_entry" {
            if entry_symbol.is_some() {
                return Err(invalid(
                    "link map must contain exactly one Rust process entry",
                ));
            }
            let address =
                u64::from_str_radix(address, 16).map_err(|error| invalid(error.to_string()))?;
            if address != entry {
                return Err(invalid(
                    "PE entry does not match Rust process entry address",
                ));
            }
            if !origin.ends_with(".rcgu.o") {
                return Err(invalid(
                    "process entry lacks Rust compiler object provenance",
                ));
            }
            entry_symbol = Some(section.to_owned());
        }
        if section.len() != 13
            || section.as_bytes().get(4) != Some(&b':')
            || address.len() != 16
            || !address.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            continue;
        }
        origins += 1;
        let metadata =
            (origin == "<absolute>" && section == "0000:00000000" && address == "0000000000000000")
                || (origin == "<linker-defined>"
                    && matches!(symbol, "__ImageBase" | "__volatile_metadata"));
        let rust = origin.ends_with(".rcgu.o");
        let os = origin
            .rsplit_once(':')
            .is_some_and(|(_, member)| crate::pe_imports::os_dll(&member.to_ascii_lowercase()));
        if !metadata && !rust && !os {
            return Err(invalid(format!(
                "unadmitted linker implementation origin: {origin} ({symbol})"
            )));
        }
    }
    if origins == 0 {
        return Err(invalid("link map contains no symbol origins"));
    }
    if entry_symbol.is_none() || entry_symbol != entry_marker {
        return Err(invalid(
            "link map entry section differs from Rust symbol or is absent",
        ));
    }
    Ok((
        format!("0x{entry:016x}"),
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    ))
}
pub fn find_map(profile: &Path, binary: &str) -> io::Result<std::path::PathBuf> {
    let map = profile
        .join("deps")
        .join(format!("{}.map", binary.replace('-', "_")));
    if !map.is_file() {
        return Err(invalid(format!(
            "actual {binary} executable link map is missing"
        )));
    }
    Ok(map)
}
#[cfg(test)]
mod tests {
    use super::*;
    const MAP:&str="0001:00000020 susi_pe_entry 0000000140001020 f libsusi_windows_runtime:entry.rcgu.o\n entry point at 0001:00000020\n";
    #[test]
    fn streamed_map_hash_and_limits_match_exact_evidence() {
        let (_, hash) =
            validate_reader(0x140001020, std::io::Cursor::new(MAP), MAP.len() as u64).unwrap();
        use sha2::{Digest, Sha256};
        let expected: String = Sha256::digest(MAP.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(hash, expected);
        assert!(
            validate_reader(0x140001020, std::io::Cursor::new(MAP), MAP.len() as u64 - 1).is_err()
        );
        let oversized_line = "x".repeat(1024 * 1024 + 1);
        assert!(validate_reader(
            0x140001020,
            std::io::Cursor::new(oversized_line),
            256 * 1024 * 1024
        )
        .is_err());
    }
    #[test]
    fn entry_requires_the_native_pe_machine() {
        let mut image = vec![0u8; 512];
        image[..2].copy_from_slice(b"MZ");
        image[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
        image[64..68].copy_from_slice(b"PE\0\0");
        image[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
        image[88 + 16..88 + 20].copy_from_slice(&0x1020_u32.to_le_bytes());
        image[88 + 24..88 + 32].copy_from_slice(&0x1_4000_0000_u64.to_le_bytes());
        let native = crate::pe_imports::native_machine().unwrap();
        image[68..70].copy_from_slice(&native.to_le_bytes());
        assert_eq!(entry_address(&image).unwrap(), 0x1_4000_1020);
        for foreign in [
            crate::pe_imports::MACHINE_AMD64,
            crate::pe_imports::MACHINE_ARM64,
            0x014c,
        ] {
            if foreign != native {
                image[68..70].copy_from_slice(&foreign.to_le_bytes());
                assert!(entry_address(&image).is_err(), "0x{foreign:04x}");
            }
        }
    }
    #[test]
    fn map_requires_matching_entry_and_no_foreign_crt_members() {
        assert!(validate_map(0x140001020, MAP).is_ok());
        assert!(validate_map(0x140001021, MAP).is_err());
        assert!(validate_map(0x140001020, &MAP.replace("entry.rcgu.o", "entry.obj")).is_err());
        for name in [
            "msvcrt.lib:exe_main.obj",
            "libucrt.lib:malloc.obj",
            "__scrt_common_main_seh",
        ] {
            assert!(validate_map(0x140001020, &format!("{MAP}{name}")).is_err());
        }
        assert!(validate_map(0x140001020, &format!("{MAP}{MAP}")).is_err());
        for origin in [
            "renamed.lib:innocent.obj",
            "private_archive:foreign.o",
            "unknown:plugin.dll",
        ] {
            assert!(validate_map(
                0x140001020,
                &format!("{MAP}0001:00000030 hidden_code 0000000140001030 f {origin}\n")
            )
            .is_err());
        }
        assert!(validate_map(
            0x140001020,
            &format!(
                "{MAP}0002:00000020 __imp_memcpy 0000000140002020 susi-os-ucrt:ucrtbase.dll\n"
            )
        )
        .is_ok());
    }
}
