//! Chrome executable discovery.
//!
//! Finds an installed Chrome/Chromium and, on Linux, resolves distro shell
//! wrappers (`/usr/bin/google-chrome`, `chromium-browser`, …) to the ELF
//! binary they exec, so launch and patching operate on the real executable.

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io::Read;

use crate::error::{Error, Result};

/// Find Chrome binary on the system
pub fn find_chrome() -> Result<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ]
    } else if cfg!(target_os = "linux") {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/google-chrome-beta",
            "/usr/bin/google-chrome-unstable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ]
    } else {
        &[]
    };

    for candidate in candidates {
        let path = Path::new(candidate);
        if path.exists() {
            #[cfg(target_os = "linux")]
            return Ok(resolve_to_elf(path, false));
            #[cfg(not(target_os = "linux"))]
            return Ok(path.to_path_buf());
        }
    }

    Err(Error::ChromeNotFound)
}

/// On Linux, `/usr/bin/chromium-browser` (and similar) are often shell script
/// wrappers, not actual ELF binaries. The patcher needs the real binary so it
/// can rewrite detection markers. This function checks the magic bytes and, if
/// the candidate is not an ELF, looks for the real binary in the standard lib
/// directories where distros place it.
#[cfg(target_os = "linux")]
pub(crate) fn resolve_to_elf(path: &Path, explicit: bool) -> PathBuf {
    if is_elf(path) {
        return path.to_path_buf();
    }

    // Follow symlinks first — google-chrome might be a symlink to the real thing
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if is_elf(&resolved) {
        tracing::info!("Resolved symlink {:?} to ELF binary {:?}", path, resolved);
        return resolved;
    }

    // Google Chrome ships `google-chrome[-beta|-unstable]` as a shell wrapper
    // that execs `$HERE/chrome` — the real ELF sits next to the wrapper. This
    // covers every channel (stable in /opt/google/chrome, beta in
    // /opt/google/chrome-beta, …) without hardcoding channel paths.
    if let Some(sibling) = resolved.parent().map(|d| d.join("chrome")) {
        if is_elf(&sibling) {
            tracing::info!("Resolved wrapper {:?} to sibling ELF {:?}", path, sibling);
            return sibling;
        }
    }

    if explicit
        && !matches!(resolved.parent(), Some(parent) if parent == Path::new("/usr/bin") || parent == Path::new("/bin"))
    {
        return path.to_path_buf();
    }

    // Derive the binary name from the candidate (e.g. "chromium-browser")
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("chromium-browser");

    let lib_candidates = [
        format!("/usr/lib/{0}/{0}", name),
        format!("/usr/lib64/{0}/{0}", name),
    ];

    for lib_path in &lib_candidates {
        let p = Path::new(lib_path);
        if is_elf(p) {
            tracing::info!("Resolved wrapper script {:?} to ELF binary {:?}", path, p);
            return p.to_path_buf();
        }
    }
    if !explicit {
        for candidate in [
            "/usr/lib/chromium-browser/chromium-browser",
            "/usr/lib/chromium/chromium",
            "/opt/google/chrome/chrome",
            "/opt/google/chrome/google-chrome",
        ] {
            let candidate = Path::new(candidate);
            if is_elf(candidate) {
                tracing::info!(
                    "Resolved discovered Chrome wrapper {:?} to ELF binary {:?}",
                    path,
                    candidate
                );
                return candidate.to_path_buf();
            }
        }
    }

    // Fallback: return as-is and let the patcher fail with a clear error
    tracing::warn!(
        "Chrome candidate {:?} is not an ELF binary and no lib binary was found",
        path,
    );
    path.to_path_buf()
}

/// Check if a file starts with the ELF magic bytes (0x7f ELF).
#[cfg(target_os = "linux")]
pub(crate) fn is_elf(path: &Path) -> bool {
    File::open(path)
        .and_then(|mut f| {
            let mut magic = [0u8; 4];
            f.read_exact(&mut magic)?;
            Ok(magic == [0x7f, b'E', b'L', b'F'])
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_chrome() {
        if let Ok(path) = find_chrome() {
            println!("Found Chrome at: {:?}", path);
            assert!(path.exists());
        }
    }

    /// Verify that find_chrome returns an ELF binary, not a shell wrapper.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_chrome_returns_elf() {
        if let Ok(path) = find_chrome() {
            assert!(
                is_elf(&path),
                "find_chrome() returned {:?} which is not an ELF binary",
                path,
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_is_elf() {
        // /usr/bin/ls should be an ELF binary on any Linux
        let ls = Path::new("/usr/bin/ls");
        if ls.exists() {
            assert!(is_elf(ls), "/usr/bin/ls should be ELF");
        }

        // A non-existent path is not ELF
        assert!(!is_elf(Path::new("/nonexistent")));
    }

    /// CI installs Chrome for Testing as `/usr/bin/google-chrome` -> a shell
    /// wrapper that execs the `chrome` ELF beside it. Discovery must return
    /// that sibling ELF, not the script.
    #[cfg(target_os = "linux")]
    #[test]
    fn symlinked_wrapper_resolves_to_sibling_elf() {
        use crate::cdp::transport::RuntimeDirectory;

        let runtime = RuntimeDirectory::create(None).unwrap();
        let install = runtime.path().join("chrome-linux64");
        fs::create_dir(&install).unwrap();
        let wrapper = install.join("google-chrome");
        fs::write(
            &wrapper,
            b"#!/bin/sh\nexec \"$(dirname \"$(readlink -f \"$0\")\")/chrome\" \"$@\"\n",
        )
        .unwrap();
        let binary = install.join("chrome");
        fs::write(&binary, b"\x7fELF fixture").unwrap();
        let bin = runtime.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let link = bin.join("google-chrome");
        std::os::unix::fs::symlink(&wrapper, &link).unwrap();

        let expected = fs::canonicalize(&binary).unwrap();
        assert_eq!(resolve_to_elf(&link, false), expected);
        assert_eq!(resolve_to_elf(&link, true), expected);
        assert_eq!(resolve_to_elf(&binary, true), binary);
    }
}
