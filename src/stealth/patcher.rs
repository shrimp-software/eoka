//! Chrome binary patcher (opt-in through `StealthConfig::patch_binary`).
//!
//! The patcher rewrites ChromeDriver's injected automation markers
//! (`$cdc_`, `$wdc_`, `$chromeDriver`) in a private copy of the browser.
//! Eoka never runs ChromeDriver, and stock Chrome contains none of these
//! markers (a scan of Chrome for Testing 155 and 156 finds zero), so on a
//! stock build the patcher only scans: it records a "clean" marker and
//! launches the original executable without copying it. Custom or forked
//! builds that do embed the markers get a patched copy.
//!
//! Rewrites are confined to non-executable, file-backed data sections (ELF
//! section headers, Mach-O sections, PE section table). A binary whose
//! headers cannot be parsed is not patched; that fails closed with an error.
//!
//! On Linux and other non-macOS Unix systems, patched copies live in a
//! persistent private cache (`$XDG_CACHE_HOME/eoka/patched-chrome` or
//! `~/.cache/eoka/patched-chrome`) keyed by the source's canonical path,
//! size, modification time and the patcher schema. Each entry is staged in
//! a temporary directory and published with one atomic rename, so concurrent
//! launches never see a partially written binary, and every reuse verifies
//! the full file length and content hash against the entry's manifest.
//! Older entries for the same source are pruned after a successful publish.
//! macOS (signed app bundles) and Windows patch into the per-launch runtime
//! directory instead, which is removed with the browser.

use aho_corasick::AhoCorasick;
#[cfg(not(target_os = "macos"))]
use memmap2::Mmap;
use memmap2::MmapMut;
#[cfg(any(test, not(target_os = "macos")))]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use crate::browser::discovery::find_chrome;
#[cfg(target_os = "linux")]
use crate::browser::discovery::{is_elf, resolve_to_elf};
use crate::error::{Error, Result};

/// ChromeDriver's automation markers. Every match inside a non-executable
/// data section is replaced by random alphanumerics of the same length.
///
/// Removed in schema 3 after scanning Chrome 155/156 (see `PATCHER_SCHEMA_VERSION`):
/// - bare `cdc_`: its only hits are the unrelated `cdc_acm` USB driver name,
///   once in `.rodata` and once as an instruction immediate in `.text`.
///   Rewriting it corrupted code and hid nothing.
/// - `--enable-automation`: only present inside a log message.
/// - `devtoolsw`, `debuggerPrivate`, `HeadlessChrome`: no hits.
/// - `Runtime.enable`, `Page.addScriptToEvaluateOnNewDocument`: detection-only
///   entries that were never rewritten.
///
/// `webdriver` is deliberately absent: Chrome builds the
/// `Navigator.prototype.webdriver` accessor name from that literal, and the
/// JS evasion already handles it.
static PATCH_PATTERNS: &[&[u8]] = &[b"$cdc_", b"$wdc_", b"$chromeDriver"];

static PATTERN_MATCHER: OnceLock<AhoCorasick> = OnceLock::new();

fn pattern_matcher() -> Result<&'static AhoCorasick> {
    if let Some(ac) = PATTERN_MATCHER.get() {
        return Ok(ac);
    }
    let ac = AhoCorasick::new(PATCH_PATTERNS).map_err(|e| {
        Error::patching(
            "init",
            format!("Failed to build Aho-Corasick automaton: {}", e),
        )
    })?;
    Ok(PATTERN_MATCHER.get_or_init(|| ac))
}

/// Bumped whenever the rewrite rules or the cache layout change. It is part
/// of the cache key and of every manifest, so older entries are never reused.
const PATCHER_SCHEMA_VERSION: u64 = 3;

/// Manifest file inside each published cache entry.
#[cfg(not(target_os = "macos"))]
const MANIFEST_NAME: &str = "eoka-patch.manifest";

/// Staging and trash leftovers from crashed processes older than this are
/// removed when another entry is published.
#[cfg(not(target_os = "macos"))]
const LEFTOVER_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

// ---------------------------------------------------------------------------
// Executable-format section parsing
// ---------------------------------------------------------------------------

fn read_bytes(data: &[u8], offset: u64, len: usize) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    data.get(start..start.checked_add(len)?)
}

fn read_u16(data: &[u8], offset: u64, big: bool) -> Option<u16> {
    let bytes: [u8; 2] = read_bytes(data, offset, 2)?.try_into().ok()?;
    Some(if big {
        u16::from_be_bytes(bytes)
    } else {
        u16::from_le_bytes(bytes)
    })
}

fn read_u32(data: &[u8], offset: u64, big: bool) -> Option<u32> {
    let bytes: [u8; 4] = read_bytes(data, offset, 4)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    })
}

fn read_u64(data: &[u8], offset: u64, big: bool) -> Option<u64> {
    let bytes: [u8; 8] = read_bytes(data, offset, 8)?.try_into().ok()?;
    Some(if big {
        u64::from_be_bytes(bytes)
    } else {
        u64::from_le_bytes(bytes)
    })
}

/// File range `[offset, offset + size)` shifted by `base`, rejected when it
/// does not fit inside `file_len`.
fn file_range(base: u64, offset: u64, size: u64, file_len: usize) -> Option<Range<usize>> {
    let start = base.checked_add(offset)?;
    let end = start.checked_add(size)?;
    if end > file_len as u64 {
        return None;
    }
    Some(start as usize..end as usize)
}

fn malformed(format: &str) -> Error {
    Error::patching(
        "sections",
        format!("Malformed {format} headers; refusing to patch the binary"),
    )
}

/// File ranges of non-executable, file-backed data sections. Patterns are only
/// ever rewritten inside these ranges, never inside code.
fn data_ranges(data: &[u8]) -> Result<Vec<Range<usize>>> {
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

fn elf_data_ranges(data: &[u8]) -> Option<Vec<Range<usize>>> {
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
fn macho64_data_ranges(data: &[u8], base: usize, len: usize) -> Option<Vec<Range<usize>>> {
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

fn fat_data_ranges(data: &[u8]) -> Option<Vec<Range<usize>>> {
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

fn pe_data_ranges(data: &[u8]) -> Option<Vec<Range<usize>>> {
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
fn patchable_matches(data: &[u8]) -> Result<Vec<Range<usize>>> {
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
fn apply_patches(data: &mut [u8]) -> Result<usize> {
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

fn fnv1a(state: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(state, |hash, byte| {
        (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(not(target_os = "macos"))]
/// Fast, stable 64-bit content hash used to detect truncated or corrupted
/// cache entries. Not a cryptographic digest: entries live in a private
/// directory owned by the current user.
fn content_hash(data: &[u8]) -> u64 {
    const MULTIPLIER: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut hash = 0xcbf2_9ce4_8422_2325 ^ data.len() as u64;
    let mut words = data.chunks_exact(8);
    for word in &mut words {
        let word = u64::from_le_bytes(word.try_into().expect("8-byte chunk"));
        hash = (hash ^ word).wrapping_mul(MULTIPLIER).rotate_left(31);
    }
    for byte in words.remainder() {
        hash = (hash ^ *byte as u64).wrapping_mul(MULTIPLIER);
    }
    hash ^ (hash >> 29)
}

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().as_bytes().to_vec()
    }
}

#[cfg(not(target_os = "macos"))]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Identity of the source executable that a cache entry was derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceIdentity {
    path: PathBuf,
    len: u64,
    mtime_ns: u128,
}

impl SourceIdentity {
    fn of(path: &Path) -> Result<Self> {
        let path = fs::canonicalize(path)?;
        let metadata = fs::metadata(&path)?;
        let mtime_ns = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        Ok(Self {
            path,
            len: metadata.len(),
            mtime_ns,
        })
    }

    fn cache_key(&self) -> String {
        let mut hash = fnv1a(0xcbf2_9ce4_8422_2325, &PATCHER_SCHEMA_VERSION.to_le_bytes());
        hash = fnv1a(hash, &path_bytes(&self.path));
        hash = fnv1a(hash, &[0]);
        hash = fnv1a(hash, &self.len.to_le_bytes());
        hash = fnv1a(hash, &self.mtime_ns.to_le_bytes());
        format!("chrome-{hash:016x}")
    }
}

/// Text manifest stored with every published entry and clean marker.
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Manifest {
    schema: u64,
    source: String,
    source_len: u64,
    source_mtime_ns: u128,
    patched_len: u64,
    patched_hash: u64,
    patches: usize,
}

#[cfg(not(target_os = "macos"))]
impl Manifest {
    fn for_source(identity: &SourceIdentity, patched_hash: u64, patches: usize) -> Self {
        Self {
            schema: PATCHER_SCHEMA_VERSION,
            source: hex(&path_bytes(&identity.path)),
            source_len: identity.len,
            source_mtime_ns: identity.mtime_ns,
            patched_len: identity.len,
            patched_hash,
            patches,
        }
    }

    fn render(&self) -> String {
        format!(
            "schema={}\nsource={}\nsource_len={}\nsource_mtime_ns={}\npatched_len={}\npatched_hash={:016x}\npatches={}\n",
            self.schema,
            self.source,
            self.source_len,
            self.source_mtime_ns,
            self.patched_len,
            self.patched_hash,
            self.patches
        )
    }

    fn parse(text: &str) -> Option<Self> {
        let mut fields = std::collections::HashMap::new();
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            if fields.insert(key, value).is_some() {
                return None;
            }
        }
        if fields.len() != 7 {
            return None;
        }
        Some(Self {
            schema: fields.get("schema")?.parse().ok()?,
            source: fields.get("source")?.to_string(),
            source_len: fields.get("source_len")?.parse().ok()?,
            source_mtime_ns: fields.get("source_mtime_ns")?.parse().ok()?,
            patched_len: fields.get("patched_len")?.parse().ok()?,
            patched_hash: u64::from_str_radix(fields.get("patched_hash")?, 16).ok()?,
            patches: fields.get("patches")?.parse().ok()?,
        })
    }

    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::symlink_metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            return None;
        }
        Self::parse(&fs::read_to_string(path).ok()?)
    }

    fn describes(&self, identity: &SourceIdentity) -> bool {
        self.schema == PATCHER_SCHEMA_VERSION
            && self.source == hex(&path_bytes(&identity.path))
            && self.source_len == identity.len
            && self.source_mtime_ns == identity.mtime_ns
    }
}

// ---------------------------------------------------------------------------
// Cache directories
// ---------------------------------------------------------------------------

/// Persistent cache root from `XDG_CACHE_HOME` (absolute) or `HOME`.
/// Pure so tests never consult or mutate the real environment.
#[cfg(all(unix, not(target_os = "macos")))]
fn persistent_cache_root_from(
    xdg_cache_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let base = xdg_cache_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(base.join("eoka").join("patched-chrome"))
}

/// The persistent cache root, or `None` where patching is per-runtime only
/// (macOS bundles, Windows) or no home directory is known.
fn persistent_cache_root() -> Option<PathBuf> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        persistent_cache_root_from(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        None
    }
}

/// Create `path` (and missing parents) private to the current user.
fn create_cache_root(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_dir_secure(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    Ok(())
}

#[cfg(unix)]
fn validate_cache_root(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    let uid = unsafe { libc::geteuid() };
    // Constructors retain the canonical root. Check every component without
    // following replacement symlinks: a private leaf is not protected when an
    // ancestor lets another user rename it.
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir()
            || (metadata.uid() != uid && metadata.uid() != 0)
            || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
        {
            return Err(Error::patching(
                "cache",
                format!("Untrusted Chrome cache ancestor: {}", ancestor.display()),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_cache_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(Error::patching(
            "cache",
            format!(
                "Chrome cache directory is not private and owned: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_cache_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let parent = path
        .parent()
        .ok_or_else(|| Error::patching("cache", "Chrome cache directory has no parent"))?;
    validate_cache_root(parent)?;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    validate_private_cache_directory(path)
}

/// Read the whole executable and count the markers the patcher would rewrite.
fn count_patchable_in_file(path: &Path) -> Result<usize> {
    Ok(patchable_matches(&fs::read(path)?)?.len())
}

/// Chrome binary patcher
pub struct ChromePatcher {
    original_path: PathBuf,
    patched_path: PathBuf,
    #[cfg(not(target_os = "macos"))]
    root: PathBuf,
    #[cfg(not(target_os = "macos"))]
    identity: SourceIdentity,
    #[cfg(target_os = "macos")]
    original_bundle: Option<PathBuf>,
    #[cfg(target_os = "macos")]
    patched_bundle: Option<PathBuf>,
}

impl ChromePatcher {
    /// Create a patcher for the given Chrome binary.
    ///
    /// Uses the persistent cache (`$XDG_CACHE_HOME/eoka/patched-chrome` or
    /// `~/.cache/eoka/patched-chrome`) on Linux and other non-macOS Unix
    /// systems, and the system temp directory elsewhere.
    pub fn new(chrome_path: &Path) -> Result<Self> {
        match persistent_cache_root() {
            Some(root) => Self::with_cache_dir(chrome_path, &root),
            None => Self::new_with_root(chrome_path, &std::env::temp_dir()),
        }
    }

    /// Create a patcher whose cache entries live under `cache_dir`. Missing
    /// directories are created private to the current user; on Unix every
    /// ancestor must be owned by the user or root and not writable by others
    /// (unless sticky).
    pub fn with_cache_dir(chrome_path: &Path, cache_dir: &Path) -> Result<Self> {
        create_cache_root(cache_dir)?;
        Self::new_with_root(chrome_path, cache_dir)
    }

    /// Patcher used by `Browser::launch`: the persistent cache where one is
    /// available and trusted, otherwise the launch's private runtime directory.
    pub(crate) fn for_launch(chrome_path: &Path, runtime: &Path) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let chrome_path = &resolve_elf_for_patching(chrome_path)?;
        if let Some(root) = persistent_cache_root() {
            match Self::with_cache_dir(chrome_path, &root) {
                Ok(patcher) => return Ok(patcher),
                Err(error) => tracing::warn!(
                    "Persistent Chrome patch cache {:?} unusable ({}); patching into the runtime directory",
                    root,
                    error
                ),
            }
        }
        Self::new_in_runtime(chrome_path, runtime)
    }

    pub(crate) fn new_in_runtime(chrome_path: &Path, runtime: &Path) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let chrome_path = &resolve_elf_for_patching(chrome_path)?;
        let root = runtime.join("patch");
        #[cfg(unix)]
        create_private_cache_directory(&root)?;
        #[cfg(not(unix))]
        create_dir_secure(&root)?;
        Self::new_with_root(chrome_path, &root)
    }

    fn new_with_root(chrome_path: &Path, root: &Path) -> Result<Self> {
        if !chrome_path.exists() {
            return Err(Error::patching(
                "new",
                format!("Chrome binary not found: {:?}", chrome_path),
            ));
        }
        #[cfg(unix)]
        let root = {
            let root = fs::canonicalize(root)?;
            validate_cache_root(&root)?;
            root
        };
        #[cfg(not(unix))]
        let root = root.to_path_buf();
        let identity = SourceIdentity::of(chrome_path)?;
        let key = identity.cache_key();

        #[cfg(target_os = "macos")]
        {
            // On macOS, handle the full .app bundle
            if let Some(bundle) = chrome_path
                .ancestors()
                .find(|path| path.extension().is_some_and(|extension| extension == "app"))
            {
                let bundle_name = bundle
                    .file_name()
                    .ok_or_else(|| Error::patching("new", "Invalid bundle path"))?;

                let patched_bundle = root.join(format!("{key}-owned")).join(bundle_name);

                let relative_path = chrome_path
                    .strip_prefix(bundle)
                    .map_err(|_| Error::patching("new", "Binary not inside bundle"))?;
                let patched_path = patched_bundle.join(relative_path);

                return Ok(Self {
                    original_path: chrome_path.to_path_buf(),
                    patched_path,
                    original_bundle: Some(bundle.to_path_buf()),
                    patched_bundle: Some(patched_bundle),
                });
            }
        }

        // Non-macOS or non-bundled binary
        let filename = chrome_path
            .file_name()
            .ok_or_else(|| Error::patching("new", "Invalid path"))?;

        let patched_path = root.join(&key).join(filename);

        Ok(Self {
            original_path: chrome_path.to_path_buf(),
            patched_path,
            #[cfg(not(target_os = "macos"))]
            root,
            #[cfg(not(target_os = "macos"))]
            identity,
            #[cfg(target_os = "macos")]
            original_bundle: None,
            #[cfg(target_os = "macos")]
            patched_bundle: None,
        })
    }

    /// Whether `get_patched_path` can answer from a verified cache entry
    /// (a patched copy, or a record that the binary needs no patching)
    /// without rewriting anything.
    pub fn is_patched(&self) -> bool {
        self.cached_path().is_some()
    }

    /// Path to launch: a verified patched copy, or the original binary when it
    /// contains nothing the patcher rewrites. Patches on a cache miss.
    pub fn get_patched_path(&self) -> Result<PathBuf> {
        if let Some(path) = self.cached_path() {
            return Ok(path);
        }
        self.patch_and_resolve()
    }

    /// Scan and, if the binary contains markers, patch now without consulting
    /// the cache first. A valid entry that a concurrent or earlier run already
    /// published is reused rather than replaced.
    pub fn patch(&self) -> Result<()> {
        self.patch_and_resolve().map(|_| ())
    }
}

#[cfg(not(target_os = "macos"))]
impl ChromePatcher {
    fn entry_directory(&self) -> &Path {
        self.patched_path
            .parent()
            .expect("patched path is always inside its entry directory")
    }

    fn entry_name(&self) -> &std::ffi::OsStr {
        self.entry_directory()
            .file_name()
            .expect("entry directory always has a name")
    }

    fn clean_marker_path(&self) -> PathBuf {
        let mut name = self.entry_name().to_os_string();
        name.push(".clean");
        self.root.join(name)
    }

    #[cfg(all(test, unix))]
    fn cache_directory(&self) -> Result<&Path> {
        Ok(self.entry_directory())
    }

    fn cached_path(&self) -> Option<PathBuf> {
        // The key was derived when the patcher was built; refuse to answer
        // for a source that has since changed.
        if SourceIdentity::of(&self.original_path).ok()? != self.identity {
            return None;
        }
        #[cfg(unix)]
        validate_cache_root(&self.root).ok()?;
        if Manifest::read(&self.clean_marker_path())
            .is_some_and(|manifest| manifest.describes(&self.identity) && manifest.patches == 0)
        {
            return Some(self.original_path.clone());
        }
        self.validate_entry(self.entry_directory())
            .ok()
            .map(|()| self.patched_path.clone())
    }

    /// Check a published (or staged) entry: private directory, manifest for
    /// this exact source, full file length and full content hash.
    fn validate_entry(&self, directory: &Path) -> Result<()> {
        #[cfg(unix)]
        validate_private_cache_directory(directory)?;
        let invalid = |reason: &str| {
            Error::patching(
                "cache",
                format!("Invalid cache entry {}: {reason}", directory.display()),
            )
        };
        let manifest =
            Manifest::read(&directory.join(MANIFEST_NAME)).ok_or_else(|| invalid("manifest"))?;
        if !manifest.describes(&self.identity) || manifest.patches == 0 {
            return Err(invalid("manifest describes a different source"));
        }
        let binary = directory.join(self.patched_path.file_name().expect("binary file name"));
        verify_binary(&binary, manifest.patched_len, manifest.patched_hash)
    }

    fn patch_and_resolve(&self) -> Result<PathBuf> {
        tracing::info!(
            "Scanning Chrome binary for patching: {:?}",
            self.original_path
        );
        #[cfg(unix)]
        validate_cache_root(&self.root)?;
        if count_patchable_in_file(&self.original_path)? == 0 {
            // Nothing to hide: launch the original and remember the result.
            // Failing to record that only costs a rescan next launch.
            match self.write_clean_marker() {
                Ok(()) => self.prune_stale_entries(),
                Err(error) => tracing::warn!("Could not record clean Chrome scan: {}", error),
            }
            tracing::info!("Chrome binary has no automation markers; using it unpatched");
            return Ok(self.original_path.clone());
        }
        for _ in 0..2 {
            let staging = self.root.join(format!(
                ".staging-{}-{}",
                std::process::id(),
                random_string(12)
            ));
            #[cfg(unix)]
            create_private_cache_directory(&staging)?;
            #[cfg(not(unix))]
            fs::create_dir(&staging)?;
            let built = self.build_entry(&staging);
            let published = built.and_then(|patches| {
                tracing::info!("Patched {patches} occurrences; publishing cache entry");
                self.publish(&staging)
            });
            if fs::symlink_metadata(&staging).is_ok() {
                let _ = fs::remove_dir_all(&staging);
            }
            match published? {
                Some(path) => {
                    self.prune_stale_entries();
                    return Ok(path);
                }
                None => continue,
            }
        }
        Err(Error::patching(
            "publish",
            "Chrome cache entry kept changing while it was being published",
        ))
    }

    /// Write the patched copy, its companions and manifest into `staging`.
    fn build_entry(&self, staging: &Path) -> Result<usize> {
        let binary = staging.join(self.patched_path.file_name().expect("binary file name"));
        let mut source = File::open(&self.original_path)?;
        let mut destination = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&binary)?;
        std::io::copy(&mut source, &mut destination)?;
        drop(source);
        if destination.metadata()?.len() != self.identity.len
            || SourceIdentity::of(&self.original_path)? != self.identity
        {
            return Err(Error::patching(
                "copy",
                "Chrome binary changed while it was being copied",
            ));
        }

        // SAFETY: `destination` is a regular file this process created with
        // create_new inside its own private staging directory; nothing else
        // maps or writes it while the map is alive.
        let mut map = unsafe { MmapMut::map_mut(&destination)? };
        let patches = apply_patches(&mut map)?;
        if patches == 0 {
            return Err(Error::patching(
                "patch",
                "Chrome binary changed between scanning and patching",
            ));
        }
        map.flush()?;
        let hash = content_hash(&map);
        drop(map);
        destination.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            destination.set_permissions(fs::Permissions::from_mode(0o755))?;
        }
        // Close the writable descriptor before anything can exec the file.
        drop(destination);
        verify_binary(&binary, self.identity.len, hash)?;

        self.link_companion_files(staging)?;
        let manifest = Manifest::for_source(&self.identity, hash, patches);
        write_synced(&staging.join(MANIFEST_NAME), manifest.render().as_bytes())?;
        sync_directory(staging);
        Ok(patches)
    }

    /// Atomically move `staging` into place. `Ok(None)` asks the caller to
    /// rebuild after a corrupt entry was moved aside.
    fn publish(&self, staging: &Path) -> Result<Option<PathBuf>> {
        let entry = self.entry_directory();
        match fs::rename(staging, entry) {
            Ok(()) => {
                sync_directory(&self.root);
                return Ok(Some(self.patched_path.clone()));
            }
            Err(error) if fs::symlink_metadata(entry).is_err() => return Err(error.into()),
            Err(_) => {}
        }
        // Another launch published first, or an entry is corrupt.
        if self.validate_entry(entry).is_ok() {
            return Ok(Some(self.patched_path.clone()));
        }
        // Only ever replace a private directory this user owns; anything else
        // (a symlink, a shared directory) is left untouched and fails closed.
        #[cfg(unix)]
        validate_private_cache_directory(entry)?;
        tracing::warn!("Replacing corrupt Chrome cache entry {:?}", entry);
        let trash = self.root.join(format!(".trash-{}", random_string(12)));
        // Never delete in place: a running browser may still use the entry.
        fs::rename(entry, &trash)?;
        let _ = fs::remove_dir_all(&trash);
        Ok(None)
    }

    fn write_clean_marker(&self) -> Result<()> {
        let marker = self.clean_marker_path();
        let temporary = self.root.join(format!(
            ".staging-{}-{}",
            std::process::id(),
            random_string(12)
        ));
        let manifest = Manifest::for_source(&self.identity, 0, 0);
        let written = write_synced(&temporary, manifest.render().as_bytes()).and_then(|()| {
            match fs::symlink_metadata(&marker) {
                Ok(metadata) if !metadata.is_file() => Err(Error::patching(
                    "cache",
                    format!("Chrome cache marker is not a file: {}", marker.display()),
                )),
                _ => fs::rename(&temporary, &marker).map_err(Into::into),
            }
        });
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written?;
        sync_directory(&self.root);
        Ok(())
    }

    /// Remove entries and markers for this source with another key (an older
    /// Chrome build or schema), and leftovers from crashed processes.
    fn prune_stale_entries(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        let own_entry = self.entry_name().to_os_string();
        let own_marker = self.clean_marker_path();
        let source = hex(&path_bytes(&self.identity.path));
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            let stale = if name_text.starts_with(".staging-") || name_text.starts_with(".trash-") {
                metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > LEFTOVER_MAX_AGE)
            } else if name_text.starts_with("chrome-") && name != own_entry && path != own_marker {
                let manifest = if metadata.is_dir() {
                    Manifest::read(&path.join(MANIFEST_NAME))
                } else {
                    Manifest::read(&path)
                };
                manifest.is_some_and(|manifest| manifest.source == source)
            } else {
                false
            };
            if !stale {
                continue;
            }
            if metadata.is_dir() {
                tracing::debug!("Pruning stale Chrome cache entry {:?}", path);
                let trash = self.root.join(format!(".trash-{}", random_string(12)));
                if fs::rename(&path, &trash).is_ok() {
                    let _ = fs::remove_dir_all(&trash);
                }
            } else {
                let _ = fs::remove_file(&path);
            }
        }
    }

    /// Link (Unix) or copy (Windows) every sibling of the original binary into
    /// `directory`. Chromium resolves shared libraries and data files
    /// (icudtl.dat, V8 snapshots, locale paks) relative to its executable.
    fn link_companion_files(&self, directory: &Path) -> Result<()> {
        let source_directory = self.original_path.parent().ok_or_else(|| {
            Error::patching("dependencies", "Chrome binary has no parent directory")
        })?;
        let binary_name = self.original_path.file_name();
        for entry in fs::read_dir(source_directory)? {
            let entry = entry?;
            if Some(entry.file_name().as_os_str()) == binary_name {
                continue;
            }
            let destination = directory.join(entry.file_name());
            if fs::symlink_metadata(&destination).is_ok() {
                continue;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(entry.path(), &destination)?;
            #[cfg(windows)]
            copy_dependency(&entry.path(), &destination)?;
        }
        Ok(())
    }
}

/// Verify the complete file: exact length and content hash.
#[cfg(not(target_os = "macos"))]
fn verify_binary(path: &Path, expected_len: u64, expected_hash: u64) -> Result<()> {
    let invalid = |reason: &str| {
        Error::patching(
            "verify",
            format!(
                "Patched Chrome {} failed verification: {reason}",
                path.display()
            ),
        )
    };
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(invalid("not a regular file"));
    }
    if metadata.len() != expected_len {
        return Err(invalid("length mismatch"));
    }
    if expected_len == 0 {
        return Err(invalid("empty file"));
    }
    let file = File::open(path)?;
    // SAFETY: read-only map of a file inside a private cache directory owned
    // by this user; published entries are never modified in place.
    let map = unsafe { Mmap::map(&file)? };
    if map.len() as u64 != expected_len || content_hash(&map) != expected_hash {
        return Err(invalid("content hash mismatch"));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn write_synced(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

/// Best-effort fsync of a directory so a rename into it survives a crash.
#[cfg(not(target_os = "macos"))]
fn sync_directory(directory: &Path) {
    #[cfg(unix)]
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}

#[cfg(target_os = "linux")]
fn resolve_elf_for_patching(chrome_path: &Path) -> Result<PathBuf> {
    let resolved = resolve_to_elf(chrome_path, true);
    if !is_elf(&resolved) {
        return Err(Error::patching(
            "new",
            "Cannot resolve the requested Chrome wrapper to its ELF binary",
        ));
    }
    Ok(resolved)
}

#[cfg(target_os = "macos")]
impl ChromePatcher {
    fn validate_cache_directory(&self) -> Result<()> {
        let directory = self.cache_directory()?;
        let parent = directory
            .parent()
            .ok_or_else(|| Error::patching("cache", "Chrome cache directory has no parent"))?;
        validate_cache_root(parent)?;
        validate_private_cache_directory(directory)
    }
}

#[cfg(target_os = "macos")]
impl ChromePatcher {
    fn cache_directory(&self) -> Result<&Path> {
        let path = self.patched_bundle.as_ref().unwrap_or(&self.patched_path);
        path.parent()
            .ok_or_else(|| Error::patching("cache", "Chrome cache directory has no parent"))
    }

    fn cached_path(&self) -> Option<PathBuf> {
        self.validate_cache_directory().ok()?;
        let original = fs::metadata(&self.original_path)
            .and_then(|m| m.modified())
            .ok()?;
        let patched = fs::metadata(&self.patched_path)
            .and_then(|m| m.modified())
            .ok()?;
        if patched <= original || count_patchable_in_file(&self.patched_path).ok()? != 0 {
            return None;
        }
        self.verify_owned_bundle()
            .and_then(|()| self.verify_signature())
            .ok()
            .map(|()| self.patched_path.clone())
    }

    fn patch_and_resolve(&self) -> Result<PathBuf> {
        tracing::info!(
            "Scanning Chrome binary for patching: {:?}",
            self.original_path
        );
        if count_patchable_in_file(&self.original_path)? == 0 {
            tracing::info!("Chrome binary has no automation markers; using it unpatched");
            return Ok(self.original_path.clone());
        }
        self.copy_bundle()?;
        if self.patched_bundle.is_none() {
            return Err(Error::patching(
                "patch",
                "Patching a macOS Chrome outside an .app bundle is unsupported",
            ));
        }
        let path = &self.patched_path;
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        // SAFETY: the file is inside the private owned bundle copy made above;
        // nothing else maps or writes it concurrently.
        let mut map = unsafe { MmapMut::map_mut(&file)? };
        let patches = apply_patches(&mut map)?;
        map.flush()?;
        drop(map);
        file.sync_all()?;
        drop(file);
        self.codesign()?;
        tracing::info!("Patched {} occurrences, saved to {:?}", patches, path);
        Ok(self.patched_path.clone())
    }

    /// Copy the app bundle into owned storage before patching or signing it.
    #[cfg(target_os = "macos")]
    fn copy_bundle(&self) -> Result<()> {
        create_private_cache_directory(self.cache_directory()?)?;
        let (orig_bundle, dest_bundle) = match (&self.original_bundle, &self.patched_bundle) {
            (Some(o), Some(d)) => (o, d),
            _ => return Ok(()),
        };

        match fs::symlink_metadata(dest_bundle) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(dest_bundle)?,
            Ok(_) => {
                return Err(Error::patching(
                    "copy_bundle",
                    "Owned Chrome bundle path is not a directory",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let orig_bundle = fs::canonicalize(orig_bundle)?;
        tracing::info!("Creating owned Chrome bundle at {:?}...", dest_bundle);
        Self::copy_owned_bundle(&orig_bundle, dest_bundle, &orig_bundle, dest_bundle)?;

        self.verify_owned_bundle()
    }

    #[cfg(target_os = "macos")]
    fn copy_owned_bundle(
        src: &Path,
        dst: &Path,
        original_bundle: &Path,
        patched_bundle: &Path,
    ) -> Result<()> {
        use std::os::unix::fs::symlink;

        fs::create_dir(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                Self::copy_owned_bundle(&src_path, &dst_path, original_bundle, patched_bundle)?;
            } else if file_type.is_file() {
                fs::copy(&src_path, &dst_path)?;
            } else if file_type.is_symlink() {
                let target = fs::read_link(&src_path)?;
                let resolved = fs::canonicalize(&src_path)?;
                let relative = resolved.strip_prefix(original_bundle).map_err(|_| {
                    Error::patching(
                        "copy_bundle",
                        format!(
                            "Bundle symlink escapes source bundle: {}",
                            src_path.display()
                        ),
                    )
                })?;
                let target = if target.is_absolute() {
                    patched_bundle.join(relative)
                } else {
                    let mut depth = src.components().count() - original_bundle.components().count();
                    for component in target.components() {
                        match component {
                            std::path::Component::ParentDir => {
                                depth = depth.checked_sub(1).ok_or_else(|| {
                                    Error::patching(
                                        "copy_bundle",
                                        format!(
                                            "Bundle symlink traverses outside source bundle: {}",
                                            src_path.display()
                                        ),
                                    )
                                })?;
                            }
                            std::path::Component::Normal(_) => depth += 1,
                            _ => {}
                        }
                    }
                    target
                };
                symlink(target, &dst_path)?;
            } else {
                return Err(Error::patching(
                    "copy_bundle",
                    format!("Unsupported bundle entry: {}", src_path.display()),
                ));
            }
        }
        fs::set_permissions(dst, fs::metadata(src)?.permissions())?;
        Ok(())
    }

    /// Validate resolved links only after the entire owned tree exists.
    /// Lexical depth alone cannot account for intermediate directory symlinks.
    #[cfg(target_os = "macos")]
    fn verify_owned_bundle(&self) -> Result<()> {
        let Some(bundle) = &self.patched_bundle else {
            return Ok(());
        };
        let bundle = fs::canonicalize(bundle)?;
        Self::verify_owned_bundle_links(&bundle, &bundle)
    }

    #[cfg(target_os = "macos")]
    fn verify_owned_bundle_links(directory: &Path, bundle: &Path) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                Self::verify_owned_bundle_links(&path, bundle)?;
            } else if file_type.is_symlink() && !fs::canonicalize(&path)?.starts_with(bundle) {
                return Err(Error::patching(
                    "copy_bundle",
                    format!("Bundle symlink escapes owned bundle: {}", path.display()),
                ));
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn codesign(&self) -> Result<()> {
        use std::process::Command;

        let sign_path = self.patched_bundle.as_ref().unwrap_or(&self.patched_path);

        tracing::info!("Re-signing {:?} with ad-hoc signature...", sign_path);

        // The copied bundle keeps extended attributes (quarantine, Finder
        // info) from the installed Chrome, and codesign rejects any bundle
        // carrying them. Strip them from the private copy first.
        let _ = Command::new("/usr/bin/xattr")
            .arg("-cr")
            .arg(sign_path)
            .output();

        // Sign without the original entitlements and hardened-runtime flags.
        // Google Chrome's entitlements include Apple-restricted ones
        // (application identifier, keychain groups, developer entitlements)
        // that only Google's signature may claim; macOS kills an ad-hoc
        // binary carrying them before it prints anything. Without the
        // hardened runtime, V8's JIT and helper loading need no entitlements.
        let output = Command::new("/usr/bin/codesign")
            .args(["--sign", "-", "--force", "--deep"])
            .arg(sign_path)
            .output()?;
        if !output.status.success() {
            return Err(Error::patching(
                "codesign",
                format!(
                    "Ad-hoc Chrome signing failed ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }
        self.verify_signature()?;
        tracing::info!("Successfully re-signed and verified patched binary");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn verify_signature(&self) -> Result<()> {
        use std::process::Command;

        let sign_path = self.patched_bundle.as_ref().unwrap_or(&self.patched_path);
        let verification = Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(sign_path)
            .output()?;
        if !verification.status.success() {
            return Err(Error::patching(
                "codesign",
                format!(
                    "Patched Chrome signature verification failed ({}): {}",
                    verification.status,
                    String::from_utf8_lossy(&verification.stderr)
                ),
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
fn copy_dependency(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_dependency(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else {
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn random_string(len: usize) -> String {
    (0..len)
        .map(|_| {
            let idx = fastrand::u8(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod fixtures {
    /// Minimal little-endian ELF64 with an executable `.text` and a data
    /// `.rodata` section (section headers only, which is all the patcher reads).
    pub(crate) fn elf(text: &[u8], rodata: &[u8]) -> Vec<u8> {
        let text_offset = 0x40u64;
        let rodata_offset = text_offset + text.len() as u64;
        let shoff = rodata_offset + rodata.len() as u64;
        let mut out = vec![0u8; 0x40];
        out[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1]);
        out[16..18].copy_from_slice(&2u16.to_le_bytes());
        out[18..20].copy_from_slice(&0x3eu16.to_le_bytes());
        out[20..24].copy_from_slice(&1u32.to_le_bytes());
        out[0x28..0x30].copy_from_slice(&shoff.to_le_bytes());
        out[0x34..0x36].copy_from_slice(&0x40u16.to_le_bytes());
        out[0x3a..0x3c].copy_from_slice(&0x40u16.to_le_bytes());
        out[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
        out.extend_from_slice(text);
        out.extend_from_slice(rodata);
        out.extend_from_slice(&[0u8; 0x40]);
        for (flags, offset, size) in [
            (0x2u64 | 0x4, text_offset, text.len() as u64),
            (0x2u64, rodata_offset, rodata.len() as u64),
        ] {
            let mut header = [0u8; 0x40];
            header[4..8].copy_from_slice(&1u32.to_le_bytes());
            header[8..16].copy_from_slice(&flags.to_le_bytes());
            header[0x18..0x20].copy_from_slice(&offset.to_le_bytes());
            header[0x20..0x28].copy_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&header);
        }
        out
    }

    /// Minimal thin Mach-O 64 with an instruction `__text` and a
    /// `__cstring` data section.
    pub(crate) fn macho(text: &[u8], cstring: &[u8]) -> Vec<u8> {
        let cmdsize = 72u32 + 2 * 80;
        let text_offset = 32 + cmdsize;
        let cstring_offset = text_offset + text.len() as u32;
        let mut out = vec![0u8; 32];
        out[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        out[16..20].copy_from_slice(&1u32.to_le_bytes());
        out[20..24].copy_from_slice(&cmdsize.to_le_bytes());
        let mut command = vec![0u8; 72];
        command[..4].copy_from_slice(&0x19u32.to_le_bytes());
        command[4..8].copy_from_slice(&cmdsize.to_le_bytes());
        command[64..68].copy_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&command);
        for (flags, offset, size) in [
            (0x8000_0400u32, text_offset, text.len() as u64),
            (0x2u32, cstring_offset, cstring.len() as u64),
        ] {
            let mut section = [0u8; 80];
            section[40..48].copy_from_slice(&size.to_le_bytes());
            section[48..52].copy_from_slice(&offset.to_le_bytes());
            section[64..68].copy_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&section);
        }
        out.extend_from_slice(text);
        out.extend_from_slice(cstring);
        out
    }

    /// Minimal PE with an executable `.text` and a data `.rdata` section.
    pub(crate) fn pe(text: &[u8], rdata: &[u8]) -> Vec<u8> {
        let pe = 0x40u32;
        let table = pe + 24;
        let text_offset = table + 2 * 40;
        let rdata_offset = text_offset + text.len() as u32;
        let mut out = vec![0u8; table as usize];
        out[..2].copy_from_slice(b"MZ");
        out[0x3c..0x40].copy_from_slice(&pe.to_le_bytes());
        out[pe as usize..pe as usize + 4].copy_from_slice(b"PE\0\0");
        out[pe as usize + 6..pe as usize + 8].copy_from_slice(&2u16.to_le_bytes());
        for (characteristics, offset, size) in [
            (0x6000_0020u32, text_offset, text.len() as u32),
            (0x4000_0040u32, rdata_offset, rdata.len() as u32),
        ] {
            let mut header = [0u8; 40];
            header[16..20].copy_from_slice(&size.to_le_bytes());
            header[20..24].copy_from_slice(&offset.to_le_bytes());
            header[36..40].copy_from_slice(&characteristics.to_le_bytes());
            out.extend_from_slice(&header);
        }
        out.extend_from_slice(text);
        out.extend_from_slice(rdata);
        out
    }

    pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{contains, elf, macho, pe};
    use super::*;

    #[test]
    fn test_random_string() {
        let s = random_string(10);
        assert_eq!(s.len(), 10);
        assert!(s.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn only_chromedriver_markers_are_rewrite_patterns() {
        let ac = pattern_matcher().unwrap();
        for marker in [&b"$cdc_"[..], b"$wdc_", b"$chromeDriver"] {
            assert!(ac.is_match(marker), "{:?}", String::from_utf8_lossy(marker));
        }
        // Scan of Chrome 155/156: these either hit unrelated data/code or
        // nothing, so they are no longer rewritten.
        for dropped in [
            &b"cdc_acm"[..],
            b"--enable-automation",
            b"devtoolsw",
            b"debuggerPrivate",
            b"HeadlessChrome",
            b"webdriver",
            b"Runtime.enable",
            b"Page.addScriptToEvaluateOnNewDocument",
        ] {
            assert!(
                !ac.is_match(dropped),
                "{:?}",
                String::from_utf8_lossy(dropped)
            );
        }
    }

    /// Regression: the old bare `cdc_` rule overwrote Chrome's inlined
    /// `cdc_acm` immediate inside `.text`. Code must never be rewritten.
    #[test]
    fn patches_never_touch_executable_sections() {
        let text = b"\xb9$cdc_\xba_acm $chromeDriver";
        let mut binary = elf(text, b"x $cdc_marker\0$wdc_ cdc_acm\0");
        let original = binary.clone();
        assert_eq!(apply_patches(&mut binary).unwrap(), 2);
        assert_eq!(binary.len(), original.len());
        assert_eq!(&binary[0x40..0x40 + text.len()], text, "code changed");
        let rodata = &binary[0x40 + text.len()..];
        assert!(!contains(rodata, b"$cdc_") && !contains(rodata, b"$wdc_"));
        assert!(contains(rodata, b"cdc_acm"), "unrelated data was rewritten");
    }

    #[test]
    fn macho_and_pe_patches_are_confined_to_data_sections() {
        for build in [macho as fn(&[u8], &[u8]) -> Vec<u8>, pe] {
            let mut binary = build(b"$cdc_code", b"$cdc_data");
            assert_eq!(apply_patches(&mut binary).unwrap(), 1);
            assert!(contains(&binary, b"$cdc_code"));
            assert!(!contains(&binary, b"$cdc_data"));
        }
        // Universal binary wrapping one thin slice.
        let thin = macho(b"$cdc_code", b"$cdc_data");
        let mut fat = vec![0u8; 0x1000];
        fat[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]);
        fat[4..8].copy_from_slice(&1u32.to_be_bytes());
        fat[16..20].copy_from_slice(&0x1000u32.to_be_bytes());
        fat[20..24].copy_from_slice(&(thin.len() as u32).to_be_bytes());
        fat.extend_from_slice(&thin);
        assert_eq!(apply_patches(&mut fat).unwrap(), 1);
        assert!(contains(&fat, b"$cdc_code") && !contains(&fat, b"$cdc_data"));
    }

    #[test]
    fn unparseable_binaries_fail_closed() {
        for data in [
            &b"#!/bin/sh\n$cdc_\n"[..],
            b"\x7fELF $cdc_",
            b"MZ $cdc_",
            b"\xcf\xfa\xed\xfe $cdc_",
        ] {
            let mut copy = data.to_vec();
            assert!(apply_patches(&mut copy).is_err(), "{data:?}");
            assert_eq!(copy, data, "a failed parse must not modify the input");
        }
        // Section header pointing past the end of the file.
        let mut binary = elf(b"code", b"$cdc_");
        let last = binary.len() - 0x40;
        binary[last + 0x20..last + 0x28].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(apply_patches(&mut binary.clone()).is_err());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn manifest_round_trips_and_rejects_garbage() {
        let identity = SourceIdentity {
            path: PathBuf::from("/opt/chrome/chrome"),
            len: 42,
            mtime_ns: 7,
        };
        let manifest = Manifest::for_source(&identity, 0xdead_beef, 3);
        assert_eq!(Manifest::parse(&manifest.render()), Some(manifest.clone()));
        assert!(manifest.describes(&identity));
        assert!(!manifest.describes(&SourceIdentity {
            len: 43,
            ..identity.clone()
        }));
        assert!(Manifest::parse("schema=3\n").is_none());
        assert!(Manifest::parse(&manifest.render().replace("patches=3", "patches=x")).is_none());
    }

    #[test]
    fn cache_key_is_stable_and_tracks_source_identity() {
        let identity = SourceIdentity {
            path: PathBuf::from("/opt/chrome/chrome"),
            len: 42,
            mtime_ns: 7,
        };
        let key = identity.cache_key();
        assert_eq!(key, identity.clone().cache_key());
        assert!(key.starts_with("chrome-") && key.len() == "chrome-".len() + 16);
        for changed in [
            SourceIdentity {
                len: 43,
                ..identity.clone()
            },
            SourceIdentity {
                mtime_ns: 8,
                ..identity.clone()
            },
            SourceIdentity {
                path: PathBuf::from("/opt/chrome2/chrome"),
                ..identity.clone()
            },
        ] {
            assert_ne!(changed.cache_key(), key);
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn persistent_cache_root_prefers_absolute_xdg_then_home() {
        use std::ffi::OsString;
        let root = |xdg: Option<&str>, home: Option<&str>| {
            persistent_cache_root_from(xdg.map(OsString::from), home.map(OsString::from))
        };
        assert_eq!(
            root(Some("/xdg"), Some("/home/u")),
            Some(PathBuf::from("/xdg/eoka/patched-chrome"))
        );
        assert_eq!(
            root(Some("relative"), Some("/home/u")),
            Some(PathBuf::from("/home/u/.cache/eoka/patched-chrome"))
        );
        assert_eq!(root(None, Some("relative")), None);
        assert_eq!(root(None, None), None);
    }
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod cache_tests {
    use super::fixtures::{contains, elf};
    use super::*;
    use crate::cdp::transport::RuntimeDirectory;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    struct Fixture {
        _runtime: RuntimeDirectory,
        source: PathBuf,
        cache: PathBuf,
        original: Vec<u8>,
    }

    fn fixture(rodata: &[u8]) -> Fixture {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let install = runtime.path().join("install");
        fs::create_dir(&install).unwrap();
        let source = install.join("chrome");
        let original = elf(b"\xb9cdc_\xba_acm", rodata);
        fs::write(&source, &original).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(install.join("icudtl.dat"), b"dependency").unwrap();
        let cache = runtime.path().join("cache");
        Fixture {
            source: fs::canonicalize(source).unwrap(),
            cache,
            original,
            _runtime: runtime,
        }
    }

    fn entries(cache: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(cache)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Stock Chrome has no markers: launch the original, copy nothing, and
    /// answer later launches from the clean marker.
    #[test]
    fn clean_binary_is_used_unpatched_and_cached() {
        let f = fixture(b"cdc_acm\0HeadlessChrome\0--enable-automation\0");
        let patcher = ChromePatcher::with_cache_dir(&f.source, &f.cache).unwrap();
        assert!(!patcher.is_patched());
        assert_eq!(patcher.get_patched_path().unwrap(), f.source);
        assert_eq!(
            entries(&f.cache),
            vec![format!("{}.clean", patcher.entry_name().to_string_lossy())]
        );
        assert!(!patcher.entry_directory().exists(), "nothing may be copied");
        let again = ChromePatcher::with_cache_dir(&f.source, &f.cache).unwrap();
        assert!(again.is_patched());
        assert_eq!(again.get_patched_path().unwrap(), f.source);
        assert_eq!(fs::read(&f.source).unwrap(), f.original);
    }

    #[test]
    fn patched_entry_is_persistent_verified_and_reused() {
        let f = fixture(b"$cdc_asdjflasutopfhvcZLmcfl_\0");
        let patcher = ChromePatcher::with_cache_dir(&f.source, &f.cache).unwrap();
        let path = patcher.get_patched_path().unwrap();
        assert_ne!(path, f.source);
        assert!(path.starts_with(fs::canonicalize(&f.cache).unwrap()));
        let patched = fs::read(&path).unwrap();
        assert_eq!(patched.len(), f.original.len());
        assert!(!contains(&patched, b"$cdc_"));
        assert!(
            contains(&patched, b"\xb9cdc_\xba_acm"),
            "code must be untouched"
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
        assert_eq!(
            fs::read(path.parent().unwrap().join("icudtl.dat")).unwrap(),
            b"dependency"
        );
        let inode = fs::metadata(&path).unwrap().ino();

        // A new patcher (a later launch) hits the cache without rewriting.
        let again = ChromePatcher::with_cache_dir(&f.source, &f.cache).unwrap();
        assert!(again.is_patched());
        assert_eq!(again.get_patched_path().unwrap(), path);
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(fs::read(&path).unwrap(), patched);
        assert!(entries(&f.cache).iter().all(|name| !name.starts_with('.')));
        assert_eq!(fs::read(&f.source).unwrap(), f.original);
    }

    #[test]
    fn truncated_or_corrupt_entries_are_rejected_and_replaced() {
        let f = fixture(b"$cdc_marker\0");
        let patcher = ChromePatcher::with_cache_dir(&f.source, &f.cache).unwrap();
        let path = patcher.get_patched_path().unwrap();
        let good = fs::read(&path).unwrap();

        // Same length, last byte flipped (the old check sampled only the
        // first 64 KiB, so corruption past it went unnoticed).
        let mut corrupt = good.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        fs::write(&path, &corrupt).unwrap();
        assert!(
            !patcher.is_patched(),
            "full-file hash must catch corruption"
        );
        let rebuilt = patcher.get_patched_path().unwrap();
        assert_eq!(rebuilt, path);
        assert!(patcher.is_patched());
        assert_eq!(fs::read(&rebuilt).unwrap().len(), good.len());

        // Truncation.
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(10).unwrap();
        drop(file);
        assert!(!patcher.is_patched());
        assert_eq!(patcher.get_patched_path().unwrap(), path);
        assert!(patcher.is_patched());
        assert!(entries(&f.cache).iter().all(|name| !name.starts_with('.')));
    }

    #[test]
    fn concurrent_patchers_publish_one_complete_entry() {
        let f = fixture(b"$cdc_marker\0$wdc_\0");
        let paths = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let patcher = ChromePatcher::with_cache_dir(&f.source, &f.cache).unwrap();
                        let path = patcher.get_patched_path().unwrap();
                        let patched = fs::read(&path).unwrap();
                        assert_eq!(patched.len(), f.original.len());
                        assert!(!contains(&patched, b"$cdc_"));
                        (path, patched)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(paths.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(entries(&f.cache).len(), 1, "{:?}", entries(&f.cache));
        assert_eq!(fs::read(&f.source).unwrap(), f.original);
    }

    #[test]
    fn updated_source_prunes_entries_for_the_previous_build() {
        let f = fixture(b"$cdc_marker\0");
        let old = ChromePatcher::with_cache_dir(&f.source, &f.cache)
            .unwrap()
            .get_patched_path()
            .unwrap();
        File::options()
            .write(true)
            .open(&f.source)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        let patcher = ChromePatcher::with_cache_dir(&f.source, &f.cache).unwrap();
        assert!(
            !patcher.is_patched(),
            "a changed source must miss the cache"
        );
        let new = patcher.get_patched_path().unwrap();
        assert_ne!(new, old);
        assert!(!old.exists(), "the previous build's copy is pruned");
        assert_eq!(entries(&f.cache).len(), 1);
    }

    #[test]
    fn private_cache_root_under_non_sticky_writable_ancestor_is_rejected() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let shared = runtime.path().join("shared");
        let private_root = shared.join("private-cache-root");
        fs::create_dir_all(&private_root).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        fs::set_permissions(&private_root, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = runtime.path().join("chrome");
        let original = elf(b"code", b"$cdc_");
        fs::write(&executable, &original).unwrap();

        assert!(
            ChromePatcher::with_cache_dir(&executable, &private_root).is_err(),
            "a private leaf must not hide a renameable, non-sticky shared ancestor"
        );
        assert_eq!(fs::read(&executable).unwrap(), original);
        assert_eq!(fs::read_dir(private_root).unwrap().count(), 0);
    }

    #[test]
    fn symlink_cache_parent_cannot_modify_source() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let source = runtime.path().join("source");
        fs::create_dir(&source).unwrap();
        let executable = source.join("chrome");
        let original = elf(b"code", b"$cdc_");
        fs::write(&executable, &original).unwrap();
        let patcher = ChromePatcher::new_with_root(&executable, runtime.path()).unwrap();
        let cache = patcher.cache_directory().unwrap();
        symlink(&source, cache).unwrap();
        let mode = fs::metadata(&source).unwrap().permissions().mode();

        assert!(!patcher.is_patched());
        assert!(patcher.patch().is_err());
        assert_eq!(fs::read(&executable).unwrap(), original);
        assert_eq!(fs::read_dir(&source).unwrap().count(), 1);
        assert_eq!(fs::metadata(&source).unwrap().permissions().mode(), mode);
        assert!(fs::symlink_metadata(cache).unwrap().is_symlink());
    }

    #[test]
    fn existing_non_private_cache_directory_is_rejected_without_chmod() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let executable = runtime.path().join("chrome");
        let original = elf(b"code", b"$cdc_");
        fs::write(&executable, &original).unwrap();
        let patcher = ChromePatcher::new_with_root(&executable, runtime.path()).unwrap();
        let cache = patcher.cache_directory().unwrap();
        fs::create_dir(cache).unwrap();
        fs::set_permissions(cache, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(cache.join("keep"), b"existing cache data").unwrap();

        assert!(!patcher.is_patched());
        assert!(patcher.patch().is_err());
        assert_eq!(
            fs::metadata(cache).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read(cache.join("keep")).unwrap(),
            b"existing cache data"
        );
        assert_eq!(fs::read(executable).unwrap(), original);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod runtime_tests {
    use super::fixtures::elf;
    use super::*;
    use crate::cdp::transport::RuntimeDirectory;

    #[test]
    fn concurrent_runtime_patchers_preserve_source_and_dependencies() {
        let root = RuntimeDirectory::create(None).unwrap();
        let source_dir = root.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        let source = source_dir.join("chrome");
        let original = elf(b"code", b"prefix $cdc_ suffix");
        fs::write(&source, &original).unwrap();
        fs::write(source_dir.join("icudtl.dat"), b"dependency").unwrap();
        let runtimes: Vec<_> = (0..8)
            .map(|_| RuntimeDirectory::create(root.path().to_str()).unwrap())
            .collect();
        let paths = std::thread::scope(|scope| {
            let workers: Vec<_> = runtimes
                .iter()
                .map(|runtime| {
                    let source = &source;
                    let original = &original;
                    scope.spawn(move || {
                        let patcher =
                            ChromePatcher::new_in_runtime(source, runtime.path()).unwrap();
                        let path = patcher.get_patched_path().unwrap();
                        assert!(path.starts_with(runtime.path()));
                        assert_eq!(
                            fs::read(path.parent().unwrap().join("icudtl.dat")).unwrap(),
                            b"dependency"
                        );
                        let patched = fs::read(&path).unwrap();
                        assert_eq!(patched.len(), original.len());
                        assert_ne!(&patched, original);
                        path
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            paths.iter().collect::<std::collections::HashSet<_>>().len(),
            paths.len()
        );
        assert_eq!(fs::read(&source).unwrap(), original);
        drop(runtimes);
        assert!(paths.iter().all(|path| !path.exists()));
        assert_eq!(fs::read(&source).unwrap(), original);
    }

    #[test]
    fn explicit_wrapper_resolution_does_not_select_an_unrelated_system_chrome() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let wrapper = runtime.path().join("custom-wrapper");
        fs::write(&wrapper, b"#!/bin/sh\nexit 1\n").unwrap();
        assert!(ChromePatcher::new_in_runtime(&wrapper, runtime.path()).is_err());
        assert!(ChromePatcher::for_launch(&wrapper, runtime.path()).is_err());
        let binary = runtime.path().join("chrome");
        fs::write(&binary, b"\x7fELF devtoolsw").unwrap();
        assert_eq!(resolve_to_elf(&wrapper, true), binary);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_bundle_tests {
    use super::fixtures::macho;
    use super::*;
    use crate::cdp::transport::RuntimeDirectory;
    use std::os::unix::fs::{symlink, MetadataExt};

    #[test]
    fn symlink_cache_parent_cannot_delete_original_bundle() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let applications = runtime.path().join("Applications");
        let source = applications.join("Chrome.app");
        let executable = source.join("Contents/MacOS/Chrome");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        let original = macho(b"code", b"$cdc_");
        fs::write(&executable, &original).unwrap();
        let resource = source.join("resource");
        fs::write(&resource, b"original bundle resource").unwrap();
        let patcher = ChromePatcher::new_with_root(&executable, runtime.path()).unwrap();
        let cache = patcher.cache_directory().unwrap();
        symlink(&applications, cache).unwrap();
        let mode = fs::metadata(&applications).unwrap().mode();
        assert_eq!(
            fs::canonicalize(patcher.patched_bundle.as_ref().unwrap()).unwrap(),
            fs::canonicalize(&source).unwrap()
        );

        assert!(!patcher.is_patched());
        assert!(patcher.patch().is_err());
        assert_eq!(fs::read(&executable).unwrap(), original);
        assert_eq!(fs::read(&resource).unwrap(), b"original bundle resource");
        assert_eq!(fs::metadata(&applications).unwrap().mode(), mode);
        assert!(fs::symlink_metadata(cache).unwrap().is_symlink());
    }

    #[test]
    fn owned_bundle_preserves_internal_links_without_sharing_source_files() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let source = runtime.path().join("source.app");
        let frameworks = source.join("Contents/Frameworks/Chrome.framework");
        fs::create_dir_all(frameworks.join("Versions/A")).unwrap();
        fs::create_dir_all(source.join("Contents/MacOS")).unwrap();
        fs::write(
            source.join("Contents/MacOS/Chrome"),
            macho(b"code", b"$cdc_"),
        )
        .unwrap();
        let resource = frameworks.join("Versions/A/resource");
        fs::write(&resource, b"verified resource").unwrap();
        symlink("A", frameworks.join("Versions/Current")).unwrap();
        symlink("Versions/Current/resource", frameworks.join("resource")).unwrap();
        symlink(&resource, source.join("Contents/absolute-resource")).unwrap();
        let destination = runtime.path().join("owned.app");
        ChromePatcher::copy_owned_bundle(&source, &destination, &source, &destination).unwrap();
        let destination = fs::canonicalize(destination).unwrap();
        ChromePatcher::verify_owned_bundle_links(&destination, &destination).unwrap();
        let owned_resource = destination.join("Contents/Frameworks/Chrome.framework/resource");
        assert_eq!(fs::read(&owned_resource).unwrap(), b"verified resource");
        assert_ne!(
            fs::metadata(&resource).unwrap().ino(),
            fs::metadata(&owned_resource).unwrap().ino()
        );
        assert!(
            fs::canonicalize(destination.join("Contents/absolute-resource"))
                .unwrap()
                .starts_with(&destination)
        );
        fs::write(&owned_resource, b"owned signing changes").unwrap();
        assert_eq!(fs::read(&resource).unwrap(), b"verified resource");
        fs::remove_dir_all(&destination).unwrap();
        assert_eq!(fs::read(&resource).unwrap(), b"verified resource");
    }

    #[test]
    fn owned_bundle_rejects_links_outside_the_source() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let source = runtime.path().join("source.app");
        fs::create_dir(&source).unwrap();
        let outside = runtime.path().join("verified-cache-file");
        fs::write(&outside, b"must not be signed").unwrap();
        symlink("../verified-cache-file", source.join("external")).unwrap();
        let destination = runtime.path().join("owned.app");
        assert!(
            ChromePatcher::copy_owned_bundle(&source, &destination, &source, &destination).is_err()
        );
        assert_eq!(fs::read(&outside).unwrap(), b"must not be signed");
    }

    #[test]
    fn owned_bundle_rejects_external_round_trips_into_source() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let source = runtime.path().join("source.app");
        fs::create_dir(&source).unwrap();
        let resource = source.join("resource");
        fs::write(&resource, b"verified resource").unwrap();
        symlink(&resource, runtime.path().join("back-in")).unwrap();
        symlink("../back-in", source.join("indirect")).unwrap();
        let destination = runtime.path().join("owned.app");
        assert!(
            ChromePatcher::copy_owned_bundle(&source, &destination, &source, &destination).is_err()
        );
        assert_eq!(fs::read(resource).unwrap(), b"verified resource");
    }

    #[test]
    fn owned_bundle_rejects_relocated_links_back_into_source_before_patching() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let source = runtime.path().join("source.app");
        let executable = source.join("Contents/MacOS/Chrome");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        let original = macho(b"code", b"$cdc_");
        fs::write(&executable, &original).unwrap();
        let source = fs::canonicalize(source).unwrap();
        let resource = source.join("resource");
        fs::write(&resource, b"must not be signed").unwrap();
        let patcher =
            ChromePatcher::new_in_runtime(&fs::canonicalize(&executable).unwrap(), runtime.path())
                .unwrap();
        let destination = patcher.patched_bundle.as_ref().unwrap();

        // The directory link resets actual resolution to the bundle root,
        // while lexical depth still counts all the preceding nested names.
        let canonical_destination = fs::canonicalize(runtime.path())
            .unwrap()
            .join(destination.strip_prefix(runtime.path()).unwrap());
        let depth = source
            .components()
            .count()
            .max(canonical_destination.components().count());
        let mut prefix = PathBuf::new();
        for _ in 1..depth {
            prefix.push("nested");
        }
        fs::create_dir_all(source.join(&prefix)).unwrap();
        symlink(&source, source.join(&prefix).join("root")).unwrap();
        let mut target = prefix.join("root");
        for _ in 0..depth {
            target.push("..");
        }
        target.push(resource.strip_prefix("/").unwrap());
        symlink(target, source.join("indirect")).unwrap();
        assert_eq!(fs::canonicalize(source.join("indirect")).unwrap(), resource);

        let error = patcher.patch().unwrap_err();
        assert!(
            error.to_string().contains("escapes owned bundle"),
            "{error}"
        );
        assert_eq!(
            fs::canonicalize(destination.join("indirect")).unwrap(),
            resource,
            "relocation must expose the otherwise accepted source-file alias"
        );
        assert_eq!(fs::read(&resource).unwrap(), b"must not be signed");
        assert_eq!(fs::read(&executable).unwrap(), original);
        assert_eq!(fs::read(&patcher.patched_path).unwrap(), original);
    }

    #[test]
    fn signing_failure_is_not_a_successful_patch() {
        let runtime = RuntimeDirectory::create(None).unwrap();
        let executable = runtime.path().join("invalid.app/Contents/MacOS/Chrome");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        let original = macho(b"code", b"$cdc_");
        fs::write(&executable, &original).unwrap();
        File::options()
            .write(true)
            .open(&executable)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        let patcher = ChromePatcher::new_in_runtime(&executable, runtime.path()).unwrap();
        assert!(patcher.patch().is_err());
        assert!(!patcher.is_patched());
        assert!(patcher.get_patched_path().is_err());
        assert_eq!(fs::read(&executable).unwrap(), original);
    }
}
