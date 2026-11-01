//! Bounded PE import inventory. No host DLL lookup can turn a redistributable
//! installed on the build machine into an admitted operating-system boundary.
use std::collections::BTreeSet;

type Result<T> = std::result::Result<T, String>;
fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        data.get(at..at + 2)
            .ok_or("truncated PE word")?
            .try_into()
            .map_err(|_| "invalid PE word")?,
    ))
}
fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(at..at + 4)
            .ok_or("truncated PE dword")?
            .try_into()
            .map_err(|_| "invalid PE dword")?,
    ))
}
pub(crate) fn os_dll(name: &str) -> bool {
    name.ends_with(".dll")
        && (name.starts_with("api-ms-win-")
            || name.starts_with("ext-ms-win-")
            || matches!(
                name,
                "kernel32.dll"
                    | "kernelbase.dll"
                    | "ntdll.dll"
                    | "advapi32.dll"
                    | "bcrypt.dll"
                    | "bcryptprimitives.dll"
                    | "crypt32.dll"
                    | "cryptbase.dll"
                    | "secur32.dll"
                    | "sspicli.dll"
                    | "ws2_32.dll"
                    | "mswsock.dll"
                    | "iphlpapi.dll"
                    | "dnsapi.dll"
                    | "user32.dll"
                    | "userenv.dll"
                    | "shell32.dll"
                    | "shlwapi.dll"
                    | "ole32.dll"
                    | "oleaut32.dll"
                    | "combase.dll"
                    | "rpcrt4.dll"
                    | "pdh.dll"
                    | "powrprof.dll"
                    | "psapi.dll"
                    | "ucrtbase.dll"
                    | "msvcrt.dll"
                    | "dbghelp.dll"
                    | "version.dll"
                    | "winhttp.dll"
                    | "wininet.dll"
                    | "normaliz.dll"
                    | "icu.dll"
                    | "netapi32.dll"
                    | "propsys.dll"
                    | "ncrypt.dll"
                    | "wintrust.dll"
                    | "gdi32.dll"
                    | "comdlg32.dll"
                    | "setupapi.dll"
                    | "cfgmgr32.dll"
                    | "wtsapi32.dll"
                    | "winmm.dll"
            ))
}

pub fn inspect(data: &[u8]) -> Result<Vec<String>> {
    if data.len() > 256 * 1024 * 1024 || data.get(..2) != Some(b"MZ") {
        return Err("invalid or oversized DOS image".into());
    }
    let pe = u32_at(data, 0x3c)? as usize;
    if pe > data.len().saturating_sub(24) {
        return Err("PE header outside file".into());
    }
    if data.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("invalid PE signature".into());
    }
    let sections = u16_at(data, pe + 6)? as usize;
    if sections == 0 || sections > 96 {
        return Err("invalid PE section count".into());
    }
    let optional_len = u16_at(data, pe + 20)? as usize;
    let optional = pe + 24;
    let (directories, count_at) = match u16_at(data, optional)? {
        0x20b => (optional + 112, optional + 108),
        0x10b => (optional + 96, optional + 92),
        _ => return Err("unsupported PE optional header".into()),
    };
    let optional_end = optional.checked_add(optional_len).ok_or("PE overflow")?;
    if optional_end > data.len() || count_at + 4 > optional_end {
        return Err("truncated PE optional header".into());
    }
    let count = u32_at(data, count_at)? as usize;
    if !(14..=16).contains(&count) || directories + count * 8 > optional_end {
        return Err("missing or malformed PE data directories".into());
    }
    let mut ranges = Vec::new();
    for section in 0..sections {
        let at = optional_end + section * 40;
        if at + 40 > data.len() {
            return Err("truncated PE section table".into());
        }
        let rva = u32_at(data, at + 12)? as usize;
        let size = u32_at(data, at + 16)? as usize;
        let file = u32_at(data, at + 20)? as usize;
        if file.checked_add(size).is_none_or(|end| end > data.len()) {
            return Err("truncated PE section data".into());
        }
        ranges.push((rva, size, file));
    }
    let map = |rva: usize, length: usize| -> Result<usize> {
        let candidates: Vec<_> = ranges
            .iter()
            .filter_map(|&(start, size, file)| {
                rva.checked_sub(start)
                    .filter(|offset| offset.checked_add(length).is_some_and(|end| end <= size))
                    .map(|offset| file + offset)
            })
            .collect();
        if candidates.len() != 1 {
            return Err("unmapped or overlapping PE RVA".into());
        }
        Ok(candidates[0])
    };
    let mut found = BTreeSet::new();
    for (index, stride, name_field) in [(1, 20, 12), (13, 32, 4)] {
        let directory = directories + index * 8;
        let rva = u32_at(data, directory)? as usize;
        let size = u32_at(data, directory + 4)? as usize;
        if rva == 0 && size == 0 {
            continue;
        }
        if rva == 0 || size < stride || size > 1024 * 1024 {
            return Err("malformed PE import directory".into());
        }
        let start = map(rva, size)?;
        let mut terminated = false;
        for offset in (0..size / stride * stride).step_by(stride) {
            let entry = &data[start + offset..start + offset + stride];
            if entry.iter().all(|byte| *byte == 0) {
                terminated = true;
                break;
            }
            if index == 13 && u32_at(entry, 0)? != 1 {
                return Err("unsupported delay-import address mode".into());
            }
            let name_rva = u32_at(entry, name_field)? as usize;
            let mut bytes = Vec::new();
            let mut end = false;
            for offset in 0..260 {
                let byte = data[map(
                    name_rva
                        .checked_add(offset)
                        .ok_or("import name RVA overflow")?,
                    1,
                )?];
                if byte == 0 {
                    end = true;
                    break;
                }
                if !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_' | b'.') {
                    return Err("invalid import DLL name".into());
                }
                bytes.push(byte.to_ascii_lowercase());
            }
            if !end || bytes.is_empty() {
                return Err("unterminated import DLL name".into());
            }
            let name = String::from_utf8(bytes).map_err(|_| "invalid DLL encoding")?;
            if !os_dll(&name) {
                return Err(format!("non-OS Windows runtime dependency: {name}"));
            }
            found.insert(format!(
                "{}:{name}",
                if index == 13 { "delay" } else { "import" }
            ));
        }
        if !terminated {
            return Err("unterminated PE import directory".into());
        }
    }
    if found.is_empty() {
        return Err("PE has no operating-system imports".into());
    }
    Ok(found.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str, delay: bool) -> Vec<u8> {
        let mut bytes = vec![0; 1024];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[70..72].copy_from_slice(&1_u16.to_le_bytes());
        bytes[84..86].copy_from_slice(&240_u16.to_le_bytes());
        bytes[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
        bytes[196..200].copy_from_slice(&16_u32.to_le_bytes());
        let directory = 200 + if delay { 13 } else { 1 } * 8;
        bytes[directory..directory + 4].copy_from_slice(&4096_u32.to_le_bytes());
        bytes[directory + 4..directory + 8]
            .copy_from_slice(&(if delay { 64_u32 } else { 40 }).to_le_bytes());
        bytes[340..344].copy_from_slice(&4096_u32.to_le_bytes());
        bytes[344..348].copy_from_slice(&512_u32.to_le_bytes());
        bytes[348..352].copy_from_slice(&512_u32.to_le_bytes());
        if delay {
            bytes[512..516].copy_from_slice(&1_u32.to_le_bytes());
        }
        let name_field = 512 + if delay { 4 } else { 12 };
        bytes[name_field..name_field + 4].copy_from_slice(&4200_u32.to_le_bytes());
        bytes[616..616 + name.len()].copy_from_slice(name.as_bytes());
        bytes
    }
    #[test]
    fn normal_and_delay_imports_admit_os_boundaries_and_refuse_redist() {
        for delay in [false, true] {
            assert!(inspect(&fixture("KERNEL32.dll", delay)).is_ok());
            for name in [
                "VCRUNTIME140.dll",
                "msvcp140.dll",
                "unknown.dll",
                "../kernel32.dll",
            ] {
                assert!(inspect(&fixture(name, delay)).is_err());
            }
        }
    }
    #[test]
    #[cfg(windows)]
    fn actual_windows_os_executable_passes() {
        let root = std::env::var_os("SystemRoot").unwrap();
        let bytes = std::fs::read(std::path::Path::new(&root).join("System32/where.exe")).unwrap();
        assert!(inspect(&bytes).is_ok(), "{:?}", inspect(&bytes));
    }
    #[test]
    fn malformed_and_truncated_images_fail_closed() {
        let bytes = fixture("kernel32.dll", false);
        for length in 0..bytes.len() {
            assert!(inspect(&bytes[..length]).is_err());
        }
        let mut bytes = fixture("kernel32.dll", true);
        bytes[512] = 0;
        assert!(inspect(&bytes).is_err());
        let mut bytes = fixture("kernel32.dll", false);
        bytes[532] = 1;
        assert!(inspect(&bytes).is_err());
    }
}
