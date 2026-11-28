//! macOS app-bundle patching.

#[allow(unused_imports)]
use super::*;

#[cfg(target_os = "macos")]
impl ChromePatcher {
    pub(super) fn validate_cache_directory(&self) -> Result<()> {
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
    pub(super) fn cache_directory(&self) -> Result<&Path> {
        let path = self.patched_bundle.as_ref().unwrap_or(&self.patched_path);
        path.parent()
            .ok_or_else(|| Error::patching("cache", "Chrome cache directory has no parent"))
    }

    pub(super) fn cached_path(&self) -> Option<PathBuf> {
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

    pub(super) fn patch_and_resolve(&self) -> Result<PathBuf> {
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
    pub(super) fn copy_bundle(&self) -> Result<()> {
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
    pub(super) fn copy_owned_bundle(
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
    pub(super) fn verify_owned_bundle(&self) -> Result<()> {
        let Some(bundle) = &self.patched_bundle else {
            return Ok(());
        };
        let bundle = fs::canonicalize(bundle)?;
        Self::verify_owned_bundle_links(&bundle, &bundle)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn verify_owned_bundle_links(directory: &Path, bundle: &Path) -> Result<()> {
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
    pub(super) fn codesign(&self) -> Result<()> {
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
    pub(super) fn verify_signature(&self) -> Result<()> {
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
