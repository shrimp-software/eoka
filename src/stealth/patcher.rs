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

use std::path::{Path, PathBuf};

pub use crate::browser::discovery::find_chrome;
use crate::error::{Error, Result};

mod cache;
#[cfg(target_os = "macos")]
mod macos;
mod sections;

#[cfg(target_os = "linux")]
use cache::resolve_elf_for_patching;
use cache::{create_cache_root, persistent_cache_root, SourceIdentity};
#[cfg(unix)]
use cache::{create_private_cache_directory, validate_cache_root};

/// Bumped whenever the rewrite rules or the cache layout change. It is part
/// of the cache key and of every manifest, so older entries are never reused.
const PATCHER_SCHEMA_VERSION: u64 = 3;

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
        create_cache_root(&root)?;
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
            let root = std::fs::canonicalize(root)?;
            validate_cache_root(&root)?;
            root
        };
        #[cfg(not(unix))]
        let root = root.to_path_buf();
        let identity = SourceIdentity::of(chrome_path)?;
        let key = identity.cache_key();

        #[cfg(target_os = "macos")]
        {
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
mod tests;
