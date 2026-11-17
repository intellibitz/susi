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
// HasWorkingDir | IsUnicode | HasExpString | PreferEnvironmentPath. The link
// opens bare `susi`, which prints usage: core has no interactive prompt.
const FLAGS: u32 = 0x0200_0290;
/// HasArguments, set only by Susi's retired encoding that launched `susi shell`.
const HAS_ARGUMENTS: u32 = 0x20;
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

/// The current link: `target` with no arguments, started in `working`.
pub(super) fn encode(target: &Path, working: &Path) -> io::Result<Vec<u8>> {
    build(target, working, None)
}

/// Susi's own earlier encoding of the same link, which launched the retired
/// interactive `susi shell`. Registration replaces it as Susi's own link.
pub(super) fn retired_shell_encoding(target: &Path, working: &Path) -> io::Result<Vec<u8>> {
    build(target, working, Some(OsStr::new("shell")))
}

fn build(target: &Path, working: &Path, argument: Option<&OsStr>) -> io::Result<Vec<u8>> {
    if !target.is_absolute() || !working.is_absolute() {
        return Err(invalid());
    }
    let flags = if argument.is_some() {
        FLAGS | HAS_ARGUMENTS
    } else {
        FLAGS
    };
    let mut data = vec![0; HEADER];
    data[..4].copy_from_slice(&(HEADER as u32).to_le_bytes());
    data[4..20].copy_from_slice(&CLSID);
    data[20..24].copy_from_slice(&flags.to_le_bytes());
    data[24..28].copy_from_slice(&0x20u32.to_le_bytes());
    data[60..64].copy_from_slice(&1u32.to_le_bytes());
    let strings = std::iter::once(compact(working)).chain(argument.map(OsStr::to_owned));
    for string in strings {
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

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = r"C:\Susi\entry\susi.exe";
    const WORKING: &str = r"C:\Susi\workspace";

    fn utf16le(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn u32_at(data: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
    }

    fn assert_invalid<T: std::fmt::Debug>(result: io::Result<T>) {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    /// Checks the fixed layout shared by both encodings and returns the
    /// offset where the EnvironmentVariableDataBlock starts.
    fn assert_layout(data: &[u8], flags: u32, strings: &[&str]) -> usize {
        assert_eq!(u32_at(data, 0), HEADER as u32);
        assert_eq!(&data[4..20], &CLSID);
        assert_eq!(u32_at(data, 20), flags);
        assert_eq!(u32_at(data, 24), 0x20, "FILE_ATTRIBUTE_ARCHIVE");
        assert_eq!(u32_at(data, 60), 1, "SW_SHOWNORMAL");
        let mut at = HEADER;
        for s in strings {
            let count = u16::from_le_bytes([data[at], data[at + 1]]);
            assert_eq!(usize::from(count), s.encode_utf16().count());
            at += 2;
            assert_eq!(
                &data[at..at + 2 * usize::from(count)],
                utf16le(s).as_slice()
            );
            at += 2 * usize::from(count);
        }
        assert_eq!(u32_at(data, at), 788);
        assert_eq!(u32_at(data, at + 4), ENV_BLOCK);
        assert_eq!(data.len(), at + 788 + 4, "block plus terminal block");
        assert_eq!(&data[data.len() - 4..], &[0, 0, 0, 0]);
        at
    }

    #[test]
    fn encode_writes_header_working_dir_and_env_block_without_arguments() {
        let data = encode(Path::new(TARGET), Path::new(WORKING)).unwrap();
        let block = assert_layout(&data, FLAGS, &[WORKING]);
        assert_eq!(FLAGS & HAS_ARGUMENTS, 0);
        // ASCII target: both the ANSI and the Unicode target fields carry it.
        assert_eq!(
            &data[block + 8..block + 8 + TARGET.len()],
            TARGET.as_bytes()
        );
        assert_eq!(data[block + 8 + TARGET.len()], 0);
        let wide_target = utf16le(TARGET);
        assert_eq!(
            &data[block + 268..block + 268 + wide_target.len()],
            wide_target.as_slice()
        );
    }

    #[test]
    fn retired_shell_encoding_adds_the_shell_argument_and_flag() {
        let data = retired_shell_encoding(Path::new(TARGET), Path::new(WORKING)).unwrap();
        assert_layout(&data, FLAGS | HAS_ARGUMENTS, &[WORKING, "shell"]);
        assert_ne!(
            data,
            encode(Path::new(TARGET), Path::new(WORKING)).unwrap(),
            "the retired link must stay distinguishable from the current one"
        );
    }

    #[test]
    fn encode_leaves_the_ansi_target_empty_for_non_ascii_paths() {
        let target = "C:\\Susi\\caf\u{e9}\\susi.exe";
        let data = encode(Path::new(target), Path::new(WORKING)).unwrap();
        let block = assert_layout(&data, FLAGS, &[WORKING]);
        assert!(data[block + 8..block + 268].iter().all(|b| *b == 0));
        let wide_target = utf16le(target);
        assert_eq!(
            &data[block + 268..block + 268 + wide_target.len()],
            wide_target.as_slice()
        );
    }

    #[test]
    fn encode_rejects_relative_target_or_working_dir() {
        assert_invalid(encode(Path::new("susi.exe"), Path::new(WORKING)));
        assert_invalid(encode(Path::new(TARGET), Path::new("workspace")));
        assert_invalid(retired_shell_encoding(
            Path::new(r"entry\susi.exe"),
            Path::new(WORKING),
        ));
    }

    #[test]
    fn wide_normalizes_separators_and_rejects_empty_long_or_nul_text() {
        let words = wide(OsStr::new("C:/Susi/entry")).unwrap();
        assert_eq!(String::from_utf16(&words).unwrap(), r"C:\Susi\entry");

        assert_invalid(wide(OsStr::new("")));
        assert!(wide(OsStr::new(&"a".repeat(MAX_UNITS - 1))).is_ok());
        assert_invalid(wide(OsStr::new(&"a".repeat(MAX_UNITS))));
        assert_invalid(wide(&OsString::from_wide(&[
            u16::from(b'A'),
            0,
            u16::from(b'B'),
        ])));
    }

    #[test]
    fn number_reads_little_endian_and_rejects_out_of_bounds() {
        let data = [1, 0, 0, 0, 0x78, 0x56, 0x34, 0x12];
        assert_eq!(number(&data, 0).unwrap(), 1);
        assert_eq!(number(&data, 4).unwrap(), 0x1234_5678);
        assert_invalid(number(&data, 5));
        assert_invalid(number(&data, 8));
        assert_invalid(number(&[], 0));
    }

    #[test]
    fn local_text_decodes_ansi_and_utf16_and_requires_a_terminator() {
        assert_eq!(local_text(b"abc\0xyz", 0, false).unwrap(), "abc");
        assert_invalid(local_text(b"abc\0xyz", 4, false)); // no terminator
        assert_invalid(local_text(&[0xC3, 0xA9, 0], 0, false)); // non-ASCII ANSI
        assert_invalid(local_text(b"a\0", 3, false)); // offset past the end

        let mut wide_bytes = utf16le("caf\u{e9}");
        wide_bytes.extend([0, 0]);
        assert_eq!(local_text(&wide_bytes, 0, true).unwrap(), "caf\u{e9}");
        assert_invalid(local_text(&utf16le("caf\u{e9}"), 0, true));
    }

    /// A LinkInfo structure with VolumeIDAndLocalBasePath set, holding
    /// `base` + `suffix` either as ANSI (header 28) or Unicode (header 36).
    fn link_info(base: &str, suffix: &str, unicode: bool) -> Vec<u8> {
        let encode = |s: &str| {
            let mut bytes = if unicode {
                utf16le(s)
            } else {
                s.as_bytes().to_vec()
            };
            bytes.extend(if unicode { &[0u8, 0][..] } else { &[0u8][..] });
            bytes
        };
        let header: u32 = if unicode { 36 } else { 28 };
        let base_bytes = encode(base);
        let suffix_bytes = encode(suffix);
        let base_at = header;
        let suffix_at = header + base_bytes.len() as u32;
        let mut section = vec![0u8; header as usize];
        section[4..8].copy_from_slice(&header.to_le_bytes());
        section[8..12].copy_from_slice(&1u32.to_le_bytes());
        if unicode {
            section[28..32].copy_from_slice(&base_at.to_le_bytes());
            section[32..36].copy_from_slice(&suffix_at.to_le_bytes());
        } else {
            section[16..20].copy_from_slice(&base_at.to_le_bytes());
            section[24..28].copy_from_slice(&suffix_at.to_le_bytes());
        }
        section.extend(base_bytes);
        section.extend(suffix_bytes);
        let size = section.len() as u32;
        section[0..4].copy_from_slice(&size.to_le_bytes());
        section
    }

    fn legacy_link(flags: u32, id_list: Option<&[u8]>, info: &[u8]) -> Vec<u8> {
        let mut data = vec![0u8; HEADER];
        data[..4].copy_from_slice(&(HEADER as u32).to_le_bytes());
        data[4..20].copy_from_slice(&CLSID);
        data[20..24].copy_from_slice(&flags.to_le_bytes());
        if let Some(ids) = id_list {
            data.extend((ids.len() as u16).to_le_bytes());
            data.extend(ids);
        }
        data.extend(info);
        data
    }

    #[test]
    fn legacy_local_target_extracts_ansi_and_unicode_local_paths() {
        let ansi = link_info(r"C:\Susi\entry", "susi.exe", false);
        assert_eq!(
            legacy_local_target(&legacy_link(2, None, &ansi)).unwrap(),
            PathBuf::from(TARGET)
        );

        let unicode = link_info(r"C:\Susi\entry\susi.exe", "", true);
        assert_eq!(
            legacy_local_target(&legacy_link(2, None, &unicode)).unwrap(),
            PathBuf::from(TARGET)
        );

        // HasLinkTargetIDList: the ID list is skipped by its own size prefix.
        let with_ids = legacy_link(1 | 2, Some(&[9, 9, 9, 9, 9, 9]), &ansi);
        assert_eq!(
            legacy_local_target(&with_ids).unwrap(),
            PathBuf::from(TARGET)
        );
    }

    #[test]
    fn legacy_local_target_rejects_foreign_or_unsupported_links() {
        let info = link_info(r"C:\Susi\entry", "susi.exe", false);

        let mut wrong_size = legacy_link(2, None, &info);
        wrong_size[0] = 75;
        assert_invalid(legacy_local_target(&wrong_size));

        let mut wrong_clsid = legacy_link(2, None, &info);
        wrong_clsid[4] ^= 0xff;
        assert_invalid(legacy_local_target(&wrong_clsid));

        assert_invalid(legacy_local_target(&legacy_link(0, None, &info))); // no HasLinkInfo
        assert_invalid(legacy_local_target(&legacy_link(2 | 0x100, None, &info))); // ForceNoLinkInfo

        let relative = link_info(r"Susi\entry", "susi.exe", false);
        assert_invalid(legacy_local_target(&legacy_link(2, None, &relative)));

        // Susi's own current link carries no LinkInfo and is never "legacy".
        let own = encode(Path::new(TARGET), Path::new(WORKING)).unwrap();
        assert_invalid(legacy_local_target(&own));

        assert_invalid(legacy_local_target(&[]));
    }
}
