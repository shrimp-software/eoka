//! Binary patcher against a real Chrome build.

use std::path::PathBuf;

use eoka::stealth::ChromePatcher;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "eoka-patcher-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn chrome() -> PathBuf {
    std::env::var_os("EOKA_TEST_CHROME_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| eoka::stealth::find_chrome().unwrap())
}

/// Stock Chrome contains none of ChromeDriver's markers: the patcher must
/// return the original executable, copy nothing, and answer the next launch
/// from its cache. Before 0.11 it copied ~300 MB per launch and overwrote an
/// unrelated `cdc_acm` instruction immediate in `.text`.
#[test]
#[ignore = "requires Chrome"]
fn stock_chrome_needs_no_patching_and_is_not_copied() {
    let chrome = std::fs::canonicalize(chrome()).unwrap();
    let before = std::fs::metadata(&chrome).unwrap();
    let cache = Directory::new();
    let root = cache.0.join("patched-chrome");

    let patcher = ChromePatcher::with_cache_dir(&chrome, &root).unwrap();
    assert!(!patcher.is_patched());
    assert_eq!(patcher.get_patched_path().unwrap(), chrome);

    let names: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with(".clean"), "{names:?}");
    assert!(std::fs::metadata(root.join(&names[0])).unwrap().len() < 4096);

    let again = ChromePatcher::with_cache_dir(&chrome, &root).unwrap();
    assert!(again.is_patched(), "the second launch must hit the cache");
    assert_eq!(again.get_patched_path().unwrap(), chrome);

    let after = std::fs::metadata(&chrome).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
}

/// `patch_binary: true` launches a working browser through the persistent
/// cache (`$XDG_CACHE_HOME/eoka/patched-chrome`; point XDG_CACHE_HOME at a
/// scratch directory to keep the run out of the real cache).
#[tokio::test]
#[ignore = "requires Chrome"]
async fn opt_in_patching_launches_a_working_browser() {
    let config = eoka::StealthConfig {
        chrome_path: Some(chrome().to_string_lossy().into_owned()),
        patch_binary: true,
        ..Default::default()
    };
    let browser = eoka::Browser::launch_with_config(config).await.unwrap();
    let page = browser.new_blank_page().await.unwrap();
    assert_eq!(
        page.evaluate::<String>("'patched-launch-ok'")
            .await
            .unwrap(),
        "patched-launch-ok"
    );
    browser.close().await.unwrap();
}
