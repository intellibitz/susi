//! Original encoder for the single local Windows Shell Link installed by Susi.
//! MS-SHLLINK §§2.1, 2.4, 2.5.4 define the header, Unicode StringData and
//! EnvironmentVariableDataBlock. Windows interprets the result directly.
//! https://learn.microsoft.com/openspecs/windows_protocols/ms-shllink/
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const HEADER: usize = 76;
const CLSID: [u8; 16] = [1, 20, 2, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 70];
const FLAGS: u32 = 0x0200_02b0;
const ENV_BLOCK: u32 = 0xa000_0001;
const MAX_UNITS: usize = 260;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "unsupported or malformed Susi Shell Link",
    )
}

fn compact(path: &Path) -> OsString {
    if let Some(relative) = std::env::var_os("LOCALAPPDATA")
        .and_then(|base| {
            path.strip_prefix(Path::new(&base))
                .ok()
                .map(Path::to_path_buf)
        })
        .filter(|relative| relative.starts_with("SusiLocal/production"))
    {
        return Path::new("%LOCALAPPDATA%").join(relative).into_os_string();
    }
    path.as_os_str().to_owned()
}

fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    // The Shell does not reliably normalize mixed '/' separators in expandable
    // targets. Emit native separators even when the caller used a mixed PathBuf.
    let words: Vec<_> = text
        .encode_wide()
        .map(|u| {
            if u == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                u
            }
        })
        .collect();
    if words.is_empty() || words.len() >= MAX_UNITS || words.contains(&0) {
        return Err(invalid());
    }
    Ok(words)
}

pub(super) fn encode(target: &Path, working: &Path) -> io::Result<Vec<u8>> {
    if !target.is_absolute() || !working.is_absolute() {
        return Err(invalid());
    }
    let mut data = vec![0; HEADER];
    data[..4].copy_from_slice(&(HEADER as u32).to_le_bytes());
    data[4..20].copy_from_slice(&CLSID);
    data[20..24].copy_from_slice(&FLAGS.to_le_bytes());
    data[24..28].copy_from_slice(&0x20u32.to_le_bytes());
    data[60..64].copy_from_slice(&1u32.to_le_bytes());
    for string in [compact(working), OsString::from("shell")] {
        let words = wide(&string)?;
        data.extend((words.len() as u16).to_le_bytes());
        data.extend(words.into_iter().flat_map(u16::to_le_bytes));
    }
    let target = compact(target);
    let words = wide(&target)?;
    let mut block = [0u8; 788];
    block[..4].copy_from_slice(&788u32.to_le_bytes());
    block[4..8].copy_from_slice(&ENV_BLOCK.to_le_bytes());
    // TargetUnicode is authoritative. The fixed ANSI field is populated only
    // when its content is representable identically on every Windows code page.
    if words.iter().all(|c| *c < 128) {
        for (slot, unit) in block[8..268].iter_mut().zip(&words) {
            *slot = *unit as u8;
        }
    }
    for (slot, unit) in block[268..].as_chunks_mut::<2>().0.iter_mut().zip(words) {
        slot.copy_from_slice(&unit.to_le_bytes());
    }
    data.extend(block);
    data.extend(0u32.to_le_bytes());
    Ok(data)
}

fn number(data: &[u8], offset: usize) -> io::Result<usize> {
    Ok(u32::from_le_bytes(
        data.get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ) as usize)
}

fn local_text(data: &[u8], offset: usize, unicode: bool) -> io::Result<OsString> {
    let bytes = data.get(offset..).ok_or_else(invalid)?;
    if unicode {
        let words = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect::<Vec<_>>();
        let end = words.iter().position(|n| *n == 0).ok_or_else(invalid)?;
        return Ok(OsString::from_wide(&words[..end]));
    }
    let end = bytes.iter().position(|c| *c == 0).ok_or_else(invalid)?;
    if !bytes[..end].is_ascii() {
        return Err(invalid());
    }
    Ok(std::str::from_utf8(&bytes[..end])
        .map_err(|_| invalid())?
        .into())
}

/// Recognize a Shell-generated direct local link only. Foreign link types,
/// relative-only links and unrecognized encodings retain their original bytes.
pub(super) fn legacy_local_target(data: &[u8]) -> io::Result<PathBuf> {
    if data.len() > 1024 * 1024 || number(data, 0)? != HEADER || data.get(4..20) != Some(&CLSID) {
        return Err(invalid());
    }
    let flags = number(data, 20)?;
    if flags & 2 == 0 || flags & 0x100 != 0 {
        return Err(invalid());
    }
    let mut offset = HEADER;
    if flags & 1 != 0 {
        let id_list = data.get(offset..offset + 2).ok_or_else(invalid)?;
        offset += 2 + usize::from(u16::from_le_bytes([id_list[0], id_list[1]]));
    }
    let candidate = data.get(offset..).ok_or_else(invalid)?;
    let section = candidate.get(..number(candidate, 0)?).ok_or_else(invalid)?;
    let header = number(section, 4)?;
    if !(header == 28 || header >= 36) || header > section.len() || number(section, 8)? & 1 == 0 {
        return Err(invalid());
    }
    let unicode = header >= 36 && number(section, 28)? != 0 && number(section, 32)? != 0;
    let base = number(section, if unicode { 28 } else { 16 })?;
    let suffix = number(section, if unicode { 32 } else { 24 })?;
    if base < header || suffix < header {
        return Err(invalid());
    }
    let mut path = PathBuf::from(local_text(section, base, unicode)?);
    let tail = local_text(section, suffix, unicode)?;
    if !tail.is_empty() {
        path.push(tail);
    }
    if !path.is_absolute() {
        return Err(invalid());
    }
    Ok(path)
}
