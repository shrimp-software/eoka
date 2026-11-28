//! Executable-format section parsing: which byte ranges may be rewritten.

#[allow(unused_imports)]
use super::*;

// ---------------------------------------------------------------------------
// Executable-format section parsing
// ---------------------------------------------------------------------------

pub(super) fn read_bytes(data: &[u8], offset: u64, len: usize) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    data.get(start..start.checked_add(len)?)
}

pub(super) fn read_u16(data: &[u8], offset: u64, big: bool) -> Option<u16> {
    let bytes: [u8; 2] = read_bytes(data, offset, 2)?.try_into().ok()?;
    Some(if big {
        u16::from_be_bytes(bytes)
    } else {
        u16::from_le_bytes(bytes)
    })
}

pub(super) fn read_u32(data: &[u8], offset: u64, big: bool) -> Option<u32> {
    let bytes: [u8; 4] = read_bytes(data, offset, 4)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    })
}

pub(super) fn read_u64(data: &[u8], offset: u64, big: bool) -> Option<u64> {
    let bytes: [u8; 8] = read_bytes(data, offset, 8)?.try_into().ok()?;
    Some(if big {
        u64::from_be_bytes(bytes)
    } else {
        u64::from_le_bytes(bytes)
    })
}

/// File range `[offset, offset + size)` shifted by `base`, rejected when it
/// does not fit inside `file_len`.
pub(super) fn file_range(
    base: u64,
    offset: u64,
    size: u64,
    file_len: usize,
) -> Option<Range<usize>> {
    let start = base.checked_add(offset)?;
    let end = start.checked_add(size)?;
    if end > file_len as u64 {
        return None;
    }
    Some(start as usize..end as usize)
}

pub(super) fn malformed(format: &str) -> Error {
    Error::patching(
        "sections",
        format!("Malformed {format} headers; refusing to patch the binary"),
    )
}

/// File ranges of non-executable, file-backed data sections. Patterns are only
/// ever rewritten inside these ranges, never inside code.
pub(super) fn data_ranges(data: &[u8]) -> Result<Vec<Range<usize>>> {
    match data.get(..4) {
        Some([0x7f, b'E', b'L', b'F']) => elf_data_ranges(data).ok_or_else(|| malformed("ELF")),
        Some([0xcf, 0xfa, 0xed, 0xfe]) => {
            macho64_data_ranges(data, 0, data.len()).ok_or_else(|| malformed("Mach-O"))
        }
        Some([0xca, 0xfe, 0xba, 0xbe]) | Some([0xca, 0xfe, 0xba, 0xbf]) => {
            fat_data_ranges(data).ok_or_else(|| malformed("universal Mach-O"))
        }
        Some([b'M', b'Z', ..]) => pe_data_ranges(data).ok_or_else(|| malformed("PE")),
        _ => Err(Error::patching(
            "sections",
            "Unrecognized executable format; refusing to patch the binary",
        )),
    }
}

pub(super) fn elf_data_ranges(data: &[u8]) -> Option<Vec<Range<usize>>> {
    const SHT_NOBITS: u32 = 8;
    const SHF_ALLOC: u64 = 0x2;
    const SHF_EXECINSTR: u64 = 0x4;

    let class = *data.get(4)?;
    let big = match *data.get(5)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    let wide = match class {
        1 => false,
        2 => true,
        _ => return None,
    };
    let (shoff, shentsize, mut shnum) = if wide {
        (
            read_u64(data, 0x28, big)?,
            read_u16(data, 0x3a, big)?,
            read_u16(data, 0x3c, big)? as u64,
        )
    } else {
        (
            read_u32(data, 0x20, big)? as u64,
            read_u16(data, 0x2e, big)?,
            read_u16(data, 0x30, big)? as u64,
        )
    };
    let min_entry = if wide { 0x40 } else { 0x28 };
    if shoff == 0 || (shentsize as u64) < min_entry {
        return None;
    }
    // Extended numbering: the real count lives in section 0's sh_size.
    if shnum == 0 {
        shnum = if wide {
            read_u64(data, shoff + 0x20, big)?
        } else {
            read_u32(data, shoff + 0x14, big)? as u64
        };
    }
    let mut ranges = Vec::new();
    for index in 0..shnum {
        let header = shoff.checked_add(index.checked_mul(shentsize as u64)?)?;
        let kind = read_u32(data, header + 4, big)?;
        let (flags, offset, size) = if wide {
            (
                read_u64(data, header + 0x08, big)?,
                read_u64(data, header + 0x18, big)?,
                read_u64(data, header + 0x20, big)?,
            )
        } else {
            (
                read_u32(data, header + 0x08, big)? as u64,
                read_u32(data, header + 0x10, big)? as u64,
                read_u32(data, header + 0x14, big)? as u64,
            )
        };
        if kind == SHT_NOBITS || flags & SHF_ALLOC == 0 || flags & SHF_EXECINSTR != 0 || size == 0 {
            continue;
        }
        ranges.push(file_range(0, offset, size, data.len())?);
    }
    Some(ranges)
}

/// One thin 64-bit Mach-O image at `base` (inside a universal binary or not).
pub(super) fn macho64_data_ranges(
    data: &[u8],
    base: usize,
    len: usize,
) -> Option<Vec<Range<usize>>> {
    const LC_SEGMENT_64: u32 = 0x19;
    const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
    const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x0000_0400;
    const ZEROFILL_TYPES: [u32; 3] = [0x1, 0xc, 0x12];

    let image = data.get(base..base.checked_add(len)?)?;
    if image.get(..4)? != [0xcf, 0xfa, 0xed, 0xfe] {
        return None;
    }
    let ncmds = read_u32(image, 16, false)?;
    let mut command = 32u64;
    let mut ranges = Vec::new();
    for _ in 0..ncmds {
        let cmd = read_u32(image, command, false)?;
        let cmdsize = read_u32(image, command + 4, false)? as u64;
        if cmdsize < 8 {
            return None;
        }
        if cmd == LC_SEGMENT_64 {
            let nsects = read_u32(image, command + 64, false)? as u64;
            if 72 + nsects.checked_mul(80)? > cmdsize {
                return None;
            }
            for index in 0..nsects {
                let section = command + 72 + index * 80;
                let size = read_u64(image, section + 40, false)?;
                let offset = read_u32(image, section + 48, false)? as u64;
                let flags = read_u32(image, section + 64, false)?;
                if ZEROFILL_TYPES.contains(&(flags & 0xff))
                    || flags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS) != 0
                    || size == 0
                {
                    continue;
                }
                ranges.push(file_range(base as u64, offset, size, base + len)?);
            }
        }
        command = command.checked_add(cmdsize)?;
    }
    Some(ranges)
}

pub(super) fn fat_data_ranges(data: &[u8]) -> Option<Vec<Range<usize>>> {
    let wide = data.get(3)? == &0xbf;
    let count = read_u32(data, 4, true)? as u64;
    let entry_size = if wide { 32 } else { 20 };
    let mut ranges = Vec::new();
    for index in 0..count {
        let entry = 8 + index * entry_size;
        let (offset, size) = if wide {
            (
                read_u64(data, entry + 8, true)?,
                read_u64(data, entry + 16, true)?,
            )
        } else {
            (
                read_u32(data, entry + 8, true)? as u64,
                read_u32(data, entry + 12, true)? as u64,
            )
        };
        let slice = file_range(0, offset, size, data.len())?;
        ranges.extend(macho64_data_ranges(data, slice.start, slice.len())?);
    }
    Some(ranges)
}

pub(super) fn pe_data_ranges(data: &[u8]) -> Option<Vec<Range<usize>>> {
    const IMAGE_SCN_CNT_CODE: u32 = 0x0000_0020;
    const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

    let pe = read_u32(data, 0x3c, false)? as u64;
    if read_bytes(data, pe, 4)? != b"PE\0\0" {
        return None;
    }
    let sections = read_u16(data, pe + 6, false)? as u64;
    let optional = read_u16(data, pe + 20, false)? as u64;
    let table = pe + 24 + optional;
    let mut ranges = Vec::new();
    for index in 0..sections {
        let header = table + index * 40;
        let size = read_u32(data, header + 16, false)? as u64;
        let offset = read_u32(data, header + 20, false)? as u64;
        let characteristics = read_u32(data, header + 36, false)?;
        if characteristics & (IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_EXECUTE) != 0 || size == 0 {
            continue;
        }
        ranges.push(file_range(0, offset, size, data.len())?);
    }
    Some(ranges)
}

/// Pattern matches that lie entirely inside a data section.
pub(super) fn patchable_matches(data: &[u8]) -> Result<Vec<Range<usize>>> {
    let ranges = data_ranges(data)?;
    let matcher = pattern_matcher()?;
    Ok(matcher
        .find_iter(data)
        .map(|m| m.range())
        .filter(|m| {
            ranges
                .iter()
                .any(|range| range.start <= m.start && m.end <= range.end)
        })
        .collect())
}

/// Rewrite every patchable match in place. Returns the number of rewrites.
pub(super) fn apply_patches(data: &mut [u8]) -> Result<usize> {
    let matches = patchable_matches(data)?;
    for range in &matches {
        let replacement = random_string(range.len());
        data[range.clone()].copy_from_slice(replacement.as_bytes());
    }
    if !matches.is_empty() {
        tracing::debug!("Rewrote {} automation markers", matches.len());
    }
    Ok(matches.len())
}

// ---------------------------------------------------------------------------
// Stable hashing (DefaultHasher is not stable across Rust releases)
// ---------------------------------------------------------------------------
