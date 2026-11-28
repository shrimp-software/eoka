//! Patcher tests.

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
mod engine {
    use super::super::*;
    use super::fixtures::{contains, elf, macho, pe};

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
    use super::super::*;
    use super::fixtures::{contains, elf};
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
    use super::super::*;
    use super::fixtures::elf;
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
    use super::super::*;
    use super::fixtures::macho;
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
