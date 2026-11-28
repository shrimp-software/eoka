//! Persistent, verified patch cache and atomic publication (non-macOS).

#[allow(unused_imports)]
use super::*;

pub(super) fn fnv1a(state: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(state, |hash, byte| {
        (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(not(target_os = "macos"))]
/// Fast, stable 64-bit content hash used to detect truncated or corrupted
/// cache entries. Not a cryptographic digest: entries live in a private
/// directory owned by the current user.
pub(super) fn content_hash(data: &[u8]) -> u64 {
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

pub(super) fn path_bytes(path: &Path) -> Vec<u8> {
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
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Identity of the source executable that a cache entry was derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceIdentity {
    pub(super) path: PathBuf,
    pub(super) len: u64,
    pub(super) mtime_ns: u128,
}

impl SourceIdentity {
    pub(super) fn of(path: &Path) -> Result<Self> {
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

    pub(super) fn cache_key(&self) -> String {
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
pub(super) struct Manifest {
    pub(super) schema: u64,
    pub(super) source: String,
    pub(super) source_len: u64,
    pub(super) source_mtime_ns: u128,
    pub(super) patched_len: u64,
    pub(super) patched_hash: u64,
    pub(super) patches: usize,
}

#[cfg(not(target_os = "macos"))]
impl Manifest {
    pub(super) fn for_source(identity: &SourceIdentity, patched_hash: u64, patches: usize) -> Self {
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

    pub(super) fn render(&self) -> String {
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

    pub(super) fn parse(text: &str) -> Option<Self> {
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

    pub(super) fn read(path: &Path) -> Option<Self> {
        let metadata = fs::symlink_metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            return None;
        }
        Self::parse(&fs::read_to_string(path).ok()?)
    }

    pub(super) fn describes(&self, identity: &SourceIdentity) -> bool {
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
pub(super) fn persistent_cache_root_from(
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
pub(super) fn persistent_cache_root() -> Option<PathBuf> {
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
pub(super) fn create_cache_root(path: &Path) -> Result<()> {
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
pub(super) fn create_dir_secure(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn validate_cache_root(path: &Path) -> Result<()> {
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
pub(super) fn validate_private_cache_directory(path: &Path) -> Result<()> {
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
pub(super) fn create_private_cache_directory(path: &Path) -> Result<()> {
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

#[cfg(not(target_os = "macos"))]
impl ChromePatcher {
    pub(super) fn entry_directory(&self) -> &Path {
        self.patched_path
            .parent()
            .expect("patched path is always inside its entry directory")
    }

    pub(super) fn entry_name(&self) -> &std::ffi::OsStr {
        self.entry_directory()
            .file_name()
            .expect("entry directory always has a name")
    }

    pub(super) fn clean_marker_path(&self) -> PathBuf {
        let mut name = self.entry_name().to_os_string();
        name.push(".clean");
        self.root.join(name)
    }

    #[cfg(all(test, unix))]
    pub(super) fn cache_directory(&self) -> Result<&Path> {
        Ok(self.entry_directory())
    }

    pub(super) fn cached_path(&self) -> Option<PathBuf> {
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
    pub(super) fn validate_entry(&self, directory: &Path) -> Result<()> {
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

    pub(super) fn patch_and_resolve(&self) -> Result<PathBuf> {
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
    pub(super) fn build_entry(&self, staging: &Path) -> Result<usize> {
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
    pub(super) fn publish(&self, staging: &Path) -> Result<Option<PathBuf>> {
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

    pub(super) fn write_clean_marker(&self) -> Result<()> {
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
    pub(super) fn prune_stale_entries(&self) {
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
    pub(super) fn link_companion_files(&self, directory: &Path) -> Result<()> {
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
pub(super) fn verify_binary(path: &Path, expected_len: u64, expected_hash: u64) -> Result<()> {
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
pub(super) fn write_synced(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

/// Best-effort fsync of a directory so a rename into it survives a crash.
#[cfg(not(target_os = "macos"))]
pub(super) fn sync_directory(directory: &Path) {
    #[cfg(unix)]
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}

#[cfg(target_os = "linux")]
pub(super) fn resolve_elf_for_patching(chrome_path: &Path) -> Result<PathBuf> {
    let resolved = resolve_to_elf(chrome_path, true);
    if !is_elf(&resolved) {
        return Err(Error::patching(
            "new",
            "Cannot resolve the requested Chrome wrapper to its ELF binary",
        ));
    }
    Ok(resolved)
}

#[cfg(windows)]
pub(super) fn copy_dependency(source: &Path, destination: &Path) -> Result<()> {
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
