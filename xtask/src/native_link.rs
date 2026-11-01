//! Bind the PE entry point to an original Rust symbol and retain the linker
//! member inventory. OS import libraries are metadata, CRT implementation
//! members remain forbidden even when their DLL imports appear OS-only.
use std::{io, path::Path};
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
pub(crate) fn entry_address(bytes: &[u8]) -> io::Result<u64> {
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
pub(crate) fn validate_map(entry: u64, map: &str) -> io::Result<String> {
    if map.len() > 64 * 1024 * 1024 {
        return Err(invalid("link map exceeds evidence budget"));
    }
    let lower = map.to_ascii_lowercase();
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
    let mut origin_rows = 0;
    for line in map.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 4
            || fields[0].len() != 13
            || !fields[0]
                .as_bytes()
                .get(4)
                .is_some_and(|byte| *byte == b':')
            || fields[2].len() != 16
            || !fields[2].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            continue;
        }
        origin_rows += 1;
        let origin = fields
            .last()
            .ok_or_else(|| invalid("missing linker origin"))?;
        let symbol = fields[1];
        let known_metadata = (*origin == "<absolute>"
            && fields[0] == "0000:00000000"
            && fields[2] == "0000000000000000")
            || (*origin == "<linker-defined>"
                && matches!(symbol, "__ImageBase" | "__volatile_metadata"));
        let rust_object = origin.ends_with(".rcgu.o");
        let os_import = origin
            .rsplit_once(':')
            .is_some_and(|(_, member)| super::pe_imports::os_dll(&member.to_ascii_lowercase()));
        if !known_metadata && !rust_object && !os_import {
            return Err(invalid(format!(
                "unadmitted linker implementation origin: {origin} ({symbol})"
            )));
        }
    }
    if origin_rows == 0 {
        return Err(invalid("link map contains no symbol origins"));
    }
    let symbols: Vec<_> = map
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.get(1) == Some(&"susi_pe_entry")).then_some(fields)
        })
        .collect();
    if symbols.len() != 1 {
        return Err(invalid(
            "link map must contain exactly one Rust process entry",
        ));
    }
    let fields = &symbols[0];
    let address = u64::from_str_radix(
        fields
            .get(2)
            .ok_or_else(|| invalid("missing entry address"))?,
        16,
    )
    .map_err(|error| invalid(error.to_string()))?;
    if address != entry {
        return Err(invalid(
            "PE entry does not match Rust process entry address",
        ));
    }
    let entry_line = map
        .lines()
        .find_map(|line| line.trim().strip_prefix("entry point at"))
        .ok_or_else(|| invalid("link map entry marker missing"))?;
    if entry_line.trim() != fields[0] {
        return Err(invalid("link map entry section differs from Rust symbol"));
    }
    if !fields.iter().any(|field| field.ends_with(".rcgu.o")) {
        return Err(invalid(
            "process entry lacks Rust compiler object provenance",
        ));
    }
    Ok(format!("0x{entry:016x}"))
}
pub(crate) fn find_map(profile: &Path, binary: &str) -> io::Result<std::path::PathBuf> {
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
