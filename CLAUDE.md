# CLAUDE.md

Guidance for Claude Code when working with this repository.

## Build & Test

Minimum supported Rust version: 1.85. CI checks the library on Rust 1.85.0.
The committed `Cargo.lock` records the dependency graph used by CI and MSRV checks.

```bash
cargo build                          # Build library
cargo build --examples               # Build examples
cargo run --example basic            # Basic usage
cargo run --example detection_test   # Bot detection tests (sannysoft, browserleaks, etc.)
cargo run --example rebrowser_test   # Rebrowser bot detector test
cargo run --example detection_test -- --visible  # Visible browser
cargo run --example request_capture  # HTTP request capture demo
```

### Tests & lint

Unit/regression tests are pure logic — no browser required.

```bash
cargo test --lib                     # all unit + regression tests
cargo t                              # alias for the above
cargo test --lib session::           # one module
cargo test --lib test_cookie_header  # one test (substring match)
cargo test --lib -- --nocapture      # show test output

cargo fmt                            # format
cargo fmt --all --check              # verify formatting (CI)
cargo clippy --all-features -- -D warnings   # lint (CI-equivalent)
cargo lint                           # alias for the above
```

Pre-push gates (mirrors CI in `.github/workflows/ci.yml`; browser checks need
Chrome and Xvfb):

```bash
cargo fmt --all --check
cargo clippy --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo build --examples --locked
cargo +1.85.0 check --lib --all-features --locked
xvfb-run -a cargo test --all-features --locked --no-fail-fast --tests -- --ignored --test-threads=1
```

CI pins Chrome for Testing 155.0.8059.39, verifies Chrome/Xvfb availability,
runs every ignored browser test without skipping failures, and uploads the log.
Cookie and lifecycle regressions use local HTTP fixtures. Chrome wrapper scripts
are resolved to their underlying ELF executable; do not skip that regression.

## Architecture

```
src/
├── lib.rs              # Public API: Browser, Page, StealthConfig, Result
├── browser.rs          # Browser API, tabs and session setup
├── browser/launch.rs   # Chrome launch preparation and profile ownership
├── page.rs             # Page abstraction, Element, request capture
├── session.rs          # Cookie import/export
├── error.rs            # Error types (ElementNotVisible, RetryExhausted, etc.)
├── cdp/
│   ├── transport.rs    # WebSocket commands, cancellation and shutdown
│   ├── transport/      # Reader, Fetch interception, Chrome process lifecycle
│   ├── connection.rs   # Connection and shared target/session state
│   ├── connection/     # Session commands grouped by page/network/document/input
│   ├── headers.rs      # Target-scoped temporary header cleanup and recovery
│   ├── network_idle.rs # Private CDP request tracking, independent of capture
│   ├── types.rs        # Protocol type re-exports
│   └── types/          # Hand-written CDP schemas grouped by domain
└── stealth/
    ├── evasions.rs     # Script selection and fingerprint substitution
    ├── evasions/scripts/ # JavaScript assets, embedded at compile time
    ├── patcher.rs      # Binary patching (Aho-Corasick)
    ├── human.rs        # Bezier curves, typing simulation
    ├── fingerprint.rs  # Fingerprint generation and persistence
    └── fingerprint/catalog.rs # Platform defaults and lookup tables
```

## Action contract (0.6)

`Page::explore(Action, ActionOptions)` is the opt-in agent exploration entry point
in 0.6.1. It preserves actionability, one deadline, durable dispatch hooks and
cleanup. Without intrinsic or explicit completion evidence, `Ok(receipt)` has
`OutcomeUnknown`; inspect the receipt. `Page::act` remains strict.

See [MIGRATION-0.6.md](MIGRATION-0.6.md). `Page::act(Action, ActionOptions)` and
`Page::locator`/`locator_text` provide receipts, strict execution-time resolution,
actionability checks, one deadline, per-target serialization and bounded held-input
cleanup. Click/type/key/submit/drag require an explicit expectation. Existing Page
and Element convenience methods use `Page::with_action_options`; Element handles
stay pinned and never retarget replacements. Fill/select/upload/focus/hover/scroll
and navigation verify intrinsic results. `with_retry` never replays action errors.
`Error::Action { code, receipt, source }` distinguishes not dispatched, uncertain,
and verified outcomes. Low-level input, JavaScript and CDP remain primitives.
`src/action/` owns this engine; persistence and file policy remain in eoka-tools.

In 0.6.2, pointer preparation scrolls clipped targets through scroll containers.
Fill supports native date/time/month/week values and browser normalization;
invalid native values and unsupported input actions fail before primary dispatch.
`Element::is_visible` and DOM visibility waits share the action visibility rules.
They do not imply enabled state or successful hit testing. Browser regressions
for these cases live in `tests/page_coverage.rs`.

## Public API Overview

### Browser
- `Browser::launch()` / `Browser::launch_with_config(config)`
- `Browser::launch_visible()` / `Browser::launch_debug()` - Common presets without manual config
- `Browser::launch_with(|config| { ... })` - Inline tweaks to default stealth config
- `browser.new_page(url)` - Create page and navigate
- `browser.tabs()` - List all open tabs (returns `Vec<TabInfo>`)
- `browser.activate_tab(id)` - Focus a tab
- `browser.close_tab(id)` - Close a specific tab
- `browser.close()`

### Page - Finding Elements
- `page.find(selector)` / `page.find_all(selector)` - By CSS selector
- `page.find_by_text(text)` - By visible text (prioritizes links/buttons)
- `page.find_by_text_match(text, TextMatch)` - With match strategy (Exact/Contains/StartsWith/EndsWith)
- `page.find_all_by_text(text)` - All elements with text
- `page.find_any(&[selectors])` - First matching selector
- `page.exists(selector)` / `page.text_exists(text)` - Check existence

### Page - Navigation
- `page.goto(url)` - Navigate to URL
- `page.goto_with_referrer(url, referrer)` - Navigate with custom Referer
- `page.goto_with_headers(url, headers)` - Target-serialized temporary headers; clears to empty after success/error/cancellation. Cleanup survives cancellation for up to five seconds. Later commands wait and retain failures until successful `clear_extra_headers()` recovery on the original session. Raw transport calls bypass this coordination.
- `page.reload()` - Reload the page
- `page.back()` / `page.forward()` - History navigation

### Page - Clicking
- `page.click(selector)` / `page.human_click(selector)` - Standard click
- `page.click_at(x, y)` - Click at coordinates
- `page.click_by_text(text)` / `page.human_click_by_text(text)` - By text
- `page.try_click(selector)` - Returns `Ok(false)` if not found/visible
- `page.try_click_by_text(text)` / `page.try_human_click(selector)` / `page.try_human_click_by_text(text)`

### Page - Form Filling
- `page.fill(selector, value)` - Clear and type
- `page.human_fill(selector, value)` - Human-like clear and type
- `page.type_text(text)` - Type into focused element
- `page.type_into(selector, text)` - Type without clearing
- `page.human_type(selector, text)` - Human-like typing

### Page - Waiting
- `page.wait_for(selector, timeout)` - Wait for element in DOM
- `page.wait_for_visible(selector, timeout)` - Wait for a rendered element with visible CSS and a nonzero box
- `page.wait_for_hidden(selector, timeout)` - Wait for element to disappear
- `page.wait_for_any(&[selectors], timeout)` - Wait for any selector
- `page.wait_for_text(text, timeout)` - Wait for text to appear
- `page.wait_for_url_contains(pattern, timeout)` - Wait for URL pattern
- `page.wait_for_url_change(timeout)` - Wait for navigation
- `page.wait_for_network_idle(idle_ms, timeout)` - Private CDP tracking of the loaded top document's fetch/XHR requests through body completion, including preexisting requests. Excludes workers/subframes/WebSockets. Previously untracked attached tabs need full navigation/reload; same-document navigation is insufficient. Tracking failures/disconnection return errors; the entire wait obeys the timeout.
- `page.wait(ms)` - Fixed delay

### Page - Info & Debug
- `page.url()` / `page.title()` / `page.content()` / `page.text()`
- `page.target_id()` - Get tab identifier (for multi-tab)
- `page.screenshot()` / `page.screenshot_jpeg(quality)`
- `page.debug_state()` - Returns `PageState` with element counts
- `page.debug_screenshot(prefix)` - Timestamped screenshot

### Page - JavaScript & Frames
- `page.evaluate(js)` / `page.evaluate_sync(js)` - Run JavaScript (sync variant skips promise await)
- `page.execute(js)` / `page.execute_sync(js)` - Run JS, discard result
- `page.frames()` - List all frames
- `page.evaluate_in_frame(frame_selector, js)` - JS in iframe (uses Function constructor, CSP-safe)

### Page - File Uploads
- `page.upload_file(selector, path)` - Upload single file
- `page.upload_files(selector, &[paths])` - Upload multiple files

### Page - Select/Dropdowns
- `page.select(selector, value)` - Select by value
- `page.select_by_text(selector, text)` - Select by visible text
- `page.select_multiple(selector, &[values])` - Multi-select

### Page - Hover
- `page.hover(selector)` - Move mouse to element (reveal menus)
- `page.human_hover(selector)` - Human-like hover

### Page - Keyboard
- `page.press_key(key)` - Press key with modifiers (`Enter`, `Ctrl+A`, `Cmd+C`)
- `page.select_all()` / `page.copy()` / `page.paste()` - Platform-aware clipboard

### Page - Network & Cookies
- `page.fetch_bounded(&BoundedFetchRequest)` - Credentialed streaming text inspection, default 8192 raw bytes, maximum 16 MiB; binary MIME bodies skipped, zero means headers only. Original `Page::fetch` is unchanged.
- `browser.watch_downloads(staging_directory)` - Owned-browser exclusive native GUID events, popup ancestry and cancellation. Subscribe before triggering, await `stop()` to restore download settings and disable target discovery; failed restoration blocks reuse. File/job policy belongs to eoka-tools. `page.navigate_for_download(url, referrer)` tolerates attachment navigation, but completion requires native events and file verification. Regression: `cargo test --lib download::`.
- `page.cookies()` / `page.set_cookie(name, value, domain, path)` / `page.delete_cookie(name, domain)`
- `page.clear_all_cookies()` / `page.set_cookies_bulk(cookies)` - Bulk operations
- `page.set_extra_headers(headers)` / `page.clear_extra_headers()` - Custom HTTP headers
- `page.enable_request_capture()` / `page.disable_request_capture()` - Network capture
- `page.get_response_body(request_id)` - Get captured response body

### Page - Configuration
- `page.set_bypass_csp(enabled)` - Disable CSP enforcement
- `page.set_javascript_enabled(enabled)` - Enable/disable JS execution (must be called before navigation)
- `page.set_user_agent(ua)` - Override User-Agent
- `page.ignore_cert_errors(ignore)` - Skip TLS verification

### Page - Dialogs
- `page.accept_dialog(prompt_text)` - Accept alert/confirm/prompt
- `page.dismiss_dialog()` - Dismiss dialog

### Page - Utilities
- `page.with_retry(attempts, delay_ms, operation)` - Retry flaky operations

### Element
- `elem.click()` / `elem.human_click()` - Click
- `elem.center()` - Get center coordinates
- `elem.type_text(text)` / `elem.focus()` - Input
- `elem.is_visible()` - Check if rendered (returns `Result<bool>`)
- `elem.bounding_box()` - Get position/size (handles rotated elements)
- `elem.get_attribute(name)` - Get attribute
- `elem.tag_name()` / `elem.value()` / `elem.text()` / `elem.outer_html()`
- `elem.is_enabled()` / `elem.is_checked()` - State
- `elem.css(property)` - Computed style
- `elem.scroll_into_view()` - Scroll into viewport

## Key Design Decisions

### CDP Command Filtering
Transport blocks detectable commands in `src/cdp/transport.rs`:
- `Runtime.enable` - BLOCKED (prevents consoleAPICalled detection)
- `Debugger.enable` - BLOCKED
- `HeapProfiler.*` - BLOCKED
- `Console.enable` - BLOCKED

### X-Client-Data Header
Chromium sends `X-Client-Data` (field-trial variation IDs) on requests to
Google-owned domains; a fresh automation profile has a distinctive set that
Google scores before any page JS runs. Eoka launches with
`--disable-field-trial-config` and strips the header via CDP Fetch
interception auto-continued in the transport reader
(`StealthConfig.strip_x_client_data`). Auto-responder CDP command IDs must
stay within int32 — DevTools silently drops commands with larger ids.

### Proxy Authentication
Configured proxy credentials answer only explicit `Proxy` challenges.
`Server`, missing, and unknown sources receive `CancelAuth` without credentials.
Without configured proxy credentials, auth events remain available to the caller.

### Document Proxy
CDP markers ($cdc_*) are hidden via Proxy on document object. See `src/stealth/evasions.rs` CDP_EVASION.

### Navigator Prototype
All navigator properties (webdriver, plugins, getBattery) are defined on `Navigator.prototype`, not the instance. This prevents detection via `Object.getOwnPropertyNames(navigator)`.

### Text Finding Priority
`find_by_text()` searches in two passes:
1. Interactive elements: `a, button, input[type="submit"], [role="button"], [onclick]`
2. Static elements: `div, span, p, label, h1-h6, li, td, th`

### Error Handling
CDP "box model" errors are converted to friendly `ElementNotVisible` errors.
Try-click methods return `Ok(false)` for both missing AND invisible elements.

## Error Types

```rust
Error::ElementNotFound(selector)      // Not in DOM
Error::ElementNotVisible { selector } // In DOM but not rendered
Error::Timeout(message)
Error::RetryExhausted { attempts, last_error }
Error::Cdp { method, code, message }  // Raw CDP error
```

## Evasion Scripts

JavaScript lives in `src/stealth/evasions/scripts/`; public constants and script
assembly remain in `src/stealth/evasions.rs`. `include_str!` embeds these assets
at compile time, so no runtime file reads are needed.

| Script | Purpose |
|--------|---------|
| `WEBDRIVER_EVASION` | navigator.webdriver = false |
| `CDP_EVASION` | Proxy on document to hide $cdc_* markers |
| `CHROME_RUNTIME_EVASION` | chrome.runtime/loadTimes/csi APIs |
| `PERMISSIONS_EVASION` | Fix Notification/Permissions consistency |
| `PLUGINS_EVASION` | Spoof navigator.plugins (3 plugins) |
| `NAVIGATOR_PROPS_EVASION` | languages, platform, hardware |
| `HEADLESS_EVASION` | Screen dimensions, Image fix |
| `BATTERY_EVASION` | navigator.getBattery() |
| `NAVIGATOR_EXTRA_EVASION` | userAgentData, connection |
| `FINGERPRINT_EVASION` | WebGL/Canvas/Audio noise |
| `WEBRTC_EVASION` | Prevent IP leak via STUN |
| `SPEECH_EVASION` | speechSynthesis.getVoices() |
| `MEDIA_DEVICES_EVASION` | mediaDevices.enumerateDevices() |
| `BLUETOOTH_EVASION` | navigator.bluetooth API |
| `TIMEZONE_EVASION` | Intl.DateTimeFormat consistency |

## Common Tasks

### Add new CDP command
1. Add types to the matching domain in `src/cdp/types/`
2. Add the `Session` method to the matching module in `src/cdp/connection/`
3. Check whether the command should be blocked/warned in `transport.rs`; keep Fetch routing policy in `transport/interception.rs`

### Add new evasion
1. Add a JavaScript asset in `src/stealth/evasions/scripts/` and its `include_str!` constant in `src/stealth/evasions.rs`
2. Add to `build_evasion_script()` function
3. Test with `cargo run --example rebrowser_test`

### Add new Page method
1. Add method to `impl Page` in `src/page.rs`
2. For text-based methods, use Runtime.callFunctionOn (no DOM mutation)
3. Update README.md API reference
4. Update this file

### Test detection bypass
```bash
cargo run --example detection_test    # sannysoft, browserleaks, creepjs
cargo run --example rebrowser_test    # Runtime.enable leak, etc.
# Check screenshots: sannysoft.png, rebrowser.png, etc.
```

## Dependencies

Minimal by design:
- `tokio` - async runtime
- `serde`/`serde_json` - serialization
- `aho-corasick` - binary patching
- `memmap2` - memory-mapped file I/O
- `fastrand` - human simulation randomness
- `base64` - screenshot/response encoding
- `thiserror` - error types
- `tracing` - logging

## Exported Types

```rust
pub use browser::{Browser, TabInfo};
pub use error::{Error, Result};
pub use action::{Action, ActionOptions, ActionReceipt, ActionOutcome, DispatchState, DispatchObserver, Expectation, Locator, LocatorQuery, LocatorSpec};
pub use network::{NetworkEvent, NetworkWatcher};
pub use page::{
    BoundingBox,      // Element position/size
    CapturedRequest,  // Network request info
    Element,          // DOM element wrapper
    FrameInfo,        // Frame/iframe info
    Page,             // Page abstraction
    PageState,        // Debug info (url, title, element counts)
    ResponseBody,     // Text or Binary response
    TextMatch,        // Exact, Contains, StartsWith, EndsWith
};
pub use session::{BrowserSession, SessionCookie};
pub use stealth::{Fingerprint, HumanSpeed};
pub struct StealthConfig { ... }
```
