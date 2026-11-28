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
cargo clippy --all-features --all-targets -- -D warnings   # lint (CI-equivalent)
cargo lint                           # alias for the above
```

Pre-push gates (mirrors CI in `.github/workflows/ci.yml`; browser checks need
Chrome and Xvfb):

```bash
cargo fmt --all --check
cargo clippy --all-features --all-targets --locked -- -D warnings
cargo test --all-features --locked
cargo build --examples --locked
cargo +1.85.0 check --lib --all-features --locked
xvfb-run -a cargo test --all-features --locked --no-fail-fast --tests -- --ignored --test-threads=1
```

CI pins Chrome for Testing in `.github/chrome-version` (156.0.8078.80), runs
every ignored browser test with nextest in two shards (`.config/nextest.toml`
retries a failure once and reports it as FLAKY) and uploads a JUnit report. CI
also runs on pushes to main, and Publish runs only after CI passes on the commit
it publishes. Cookie and lifecycle regressions use local HTTP fixtures. Chrome
wrapper scripts are resolved to their underlying ELF executable; do not skip
that regression.

All browser tests are modules of one binary, `tests/integration.rs`
(`tests/integration/*.rs`); add new ones there, not as new top-level files.
Run one file with `cargo test --test integration -- --ignored page_coverage::`.
Set `EOKA_TEST_CHROME_PATH` to the pinned Chrome (under
`~/.cache/eoka/chrome-linux-x64-gnu-156.*`); a local beta Chrome may lack WebGL
under Xvfb. Run one Chrome suite at a time.

## Architecture

```
src/
├── lib.rs              # Public API: Browser, Page, StealthConfig, Result
├── browser.rs          # Browser API, tabs and session setup
├── browser/launch.rs   # Chrome launch preparation and profile ownership
├── browser/discovery.rs # Chrome discovery and wrapper-to-ELF resolution
├── page.rs             # Page abstraction
├── page/frames.rs      # Frame snapshots, geometry and frame evaluation
├── page/frames/shadow.rs # Clicking inside (closed) shadow roots
├── page/text_candidates.js # The one text matcher (find_by_text, text locators)
├── element.rs          # Element handles, pinned to backend node ids
├── network.rs          # RequestCapture, NetworkWatcher
├── session.rs          # Cookie import/export (incl. CHIPS partition keys)
├── error.rs            # Error enum; error/code.rs (ErrorCode), error/classify.rs (CDP error text)
├── action/             # Action engine: runner, perform, locator, destination
├── cdp/
│   ├── transport.rs    # WebSocket commands, cancellation and routes
│   ├── transport/      # Connect, pending commands, reader, writer task, Fetch interception, shutdown, Chrome process lifecycle
│   ├── connection.rs   # Connection and shared target/session state
│   ├── connection/     # Session commands grouped by page/network/document/input
│   ├── headers.rs      # Target-scoped temporary header cleanup and recovery
│   ├── network_idle.rs # Private CDP request tracking, independent of capture
│   ├── types.rs        # Protocol type re-exports
│   └── types/          # Hand-written CDP schemas grouped by domain
└── stealth/
    ├── evasions.rs     # Script selection and fingerprint substitution
    ├── evasions/scripts/ # JavaScript assets, embedded at compile time
    ├── patcher.rs      # Opt-in binary patching; patcher/{sections,cache,macos}.rs
    ├── human.rs        # Bezier curves, typing simulation
    ├── fingerprint.rs  # Fingerprint generation and persistence
    └── fingerprint/catalog.rs # Platform defaults and lookup tables
```

## Action contract

`Page::explore(Action, ActionOptions)` is the opt-in agent exploration entry point.
It preserves actionability, one deadline, durable dispatch hooks and
cleanup. Without intrinsic or explicit completion evidence, `Ok(receipt)` has
`OutcomeUnknown`; inspect the receipt. `Page::act` remains strict.

Text fill/type use `InputMode::Sequential` by default and check the original
input before each character; `InputMode::Bulk` opts into one native insertion
and `InputMode::Human` (used by `human_type`/`human_fill`) sends trusted
keydown/keyup pairs at irregular intervals. Fill fails with `ValueMismatch`
(character counts only) once a different value has been stable for 750ms.
Locators always resolve in one cached isolated world per frame
(`src/action/locator.rs`), never in the page's main world.
`Expectation::DocumentReady`, `Enabled` and `All` express readiness within one
deadline. `Destination` controls current/new-tab verification, with correlated
child targets recorded in `ActionReceipt::destination_target_id`.
`Locator::state` returns a `ControlState` without input values.
`Page::evaluate_function` and `execute_function` accept a function and bounded
JSON arguments, execute once in fresh scope and propagate actual exceptions.
Function results reject non-JSON values before CDP serialization and are limited
to 1 MiB of UTF-8 JSON. `evaluate_json` and `evaluate_json_in_frame_id` reuse this
validator for raw expressions with an explicit result byte limit.

`Locator` offers click, double_click, human_click, hover, human_hover, fill, type_text, focus, submit, select, upload and scroll_into_view. `Page::act(Action, ActionOptions)` and
`Page::locator`/`locator_text` provide receipts, strict execution-time resolution,
actionability checks, one deadline, per-target serialization and bounded held-input
cleanup. Through `act` and `Locator`, click/type/key/submit/drag require an explicit
expectation. Page and Element shorthand for those actions returns `ActionCall`
(`src/action/call.rs`), which runs through `explore`: a plain `.await` proves dispatch
only (`OutcomeUnknown`), and `.expecting(..)` proves one call. `try_*` returns
`TryActionCall`, where `Ok(false)` means no input was sent (preparation may have
scrolled). `type_text` requires an editable focus. Page handles carry
`ActionDefaults` (`with_action_defaults`) and never an expectation. Element handles
stay pinned and never retarget replacements. Fill/select/upload/focus/hover/scroll
and navigation verify intrinsic results; an aborted navigation fails at once with
`navigation_aborted`, and post-dispatch checks treat a destroyed context as
"not yet". `with_retry` retries only `Error::is_retryable` errors, so it never
replays input that may have landed. `Error::Action { code: ErrorCode, receipt,
source }` distinguishes not dispatched, uncertain, and verified outcomes. Low-level input, JavaScript and CDP remain primitives.
`src/action/` owns this engine; persistence and file policy remain in eoka-tools.

Pointer preparation scrolls clipped targets through scroll containers. Fill
supports native date/time/month/week values with browser normalization; invalid
native values and unsupported input actions fail before dispatch.
`Element::is_visible` and visibility waits share the action visibility rules and
do not imply enabled state or hit testing. Browser regressions for these cases
live in `tests/page_coverage.rs`.

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
- `page.find_by_text(text)` - By visible text (interactive first, then the innermost static match)
- `page.find_by_text_match(text, TextMatch)` - With match strategy (Exact/Contains/StartsWith/EndsWith)
- `page.find_all_by_text(text)` - All elements with text
- `page.find_any(&[selectors])` - First matching selector
- `page.exists(selector)` / `page.text_exists(text)` - Check existence

### Page - Navigation
- `page.goto(url)` - Navigate to URL
- `page.goto_with_referrer(url, referrer)` - Navigate with custom Referer
- `page.goto_with_headers(url, headers)` - Target-serialized temporary headers; clears to empty after success/error/cancellation. Cleanup survives cancellation for up to five seconds. Later commands wait and retain failures until successful `clear_extra_headers()` recovery on the original session. Raw transport calls bypass this coordination.
- `page.reload()` - Reload the page
- `page.back()` / `page.forward()` - History navigation; no entry fails before dispatch with `history_unavailable`

### Page - Clicking
- `page.click(selector)` / `page.human_click(selector)` - Returns `ActionCall`: `.await` proves dispatch only, `.expecting(..)` proves the result
- `page.click_at(x, y)` - Click at coordinates
- `page.click_by_text(text)` / `page.human_click_by_text(text)` - By text
- `page.click_shadow(selector)` / `page.click_shadow_in_frame(frame_id, selector)` - Click through closed shadow roots (top frame or any child frame); maps frame-local coordinates and uses human-like movement
- `page.try_click(selector)` - Returns `TryActionCall`: `Ok(false)` if not found/actionable and no input was sent
- `page.try_click_by_text(text)` / `page.try_human_click(selector)` / `page.try_human_click_by_text(text)`

### Page - Form Filling
- `page.fill(selector, value)` - Clear and type
- `page.human_fill(selector, value)` - Clear, then type with `InputMode::Human`
- `page.type_text(text)` - Type into focused element (`ActionCall`)
- `page.type_into(selector, text)` - Type without clearing (`ActionCall`)
- `page.human_type(selector, text)` - Type with `InputMode::Human` (`ActionCall`)

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
- `page.screenshot_with_options(&ScreenshotOptions)` - Typed PNG/JPEG/WebP, document-relative CSS crop or full page; PNG rejects quality, JPEG/WebP accept 0–100. Crop and full page conflict; viewport DPR is the single render scale.
- `page.start_recording(NativeRecordingOptions)` - Native Chrome MP4 handle; stop, read chunks up to 64 KiB through EOF, close; explicit cancel and bounded best-effort Drop cleanup. Active/ready page and one recorder per target; unsupported fails without fallback.
- `page.set_viewport(width, height, device_scale_factor)` - CSS viewport dimensions and finite positive DPR; `640, 360, 2.0` captures 1280×720 physical pixels. Non-live sessions apply configured viewport/DPR at setup; live attachment preserves existing metrics.
- `page.reset_viewport()` - Clear emulation and restore native window content dimensions/DPR, not the launch override. Overrides and resets affect all handles to the same tab and persist across navigation.
- `page.debug_state()` - Returns `PageState` with element counts
- `page.debug_screenshot(prefix)` - Timestamped screenshot

### Page - JavaScript & Frames
- `page.evaluate(js)` / `page.evaluate_sync(js)` - Run JavaScript (sync variant skips promise await)
- `page.execute(js)` / `page.execute_sync(js)` - Run JS, discard result
- `page.frames()` - List all frames
- `page.evaluate_in_frame(frame_selector, js)` - JS in iframe (runs in an isolated world, CSP-safe)

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
- `page.press_key(key)` - Press key with modifiers (`Enter`, `Ctrl+A`, `Cmd+C`); returns `ActionCall`
- `page.select_all()` / `page.copy()` / `page.paste()` - Platform-aware clipboard (`ActionCall`)

### Page - Network & Cookies
- `page.fetch_bounded(&BoundedFetchRequest)` - Credentialed streaming text inspection, default 8192 raw bytes, maximum 16 MiB; binary MIME bodies skipped, zero means headers only. Original `Page::fetch` is unchanged.
- `browser.watch_downloads(staging_directory)` - Owned-browser exclusive native GUID events, popup ancestry and cancellation. Subscribe before triggering, await `stop()` to restore download settings while keeping shared target discovery active; failed restoration blocks reuse. File/job policy belongs to eoka-tools. `page.navigate_for_download(url, referrer)` tolerates attachment navigation, but completion requires native events and file verification. Regression: `cargo test --lib download::`.
- `page.capture_state()` / `page.restore_state(state)` - Like `storageState`: all sites' cookies in the page's browser context, current origin's storage; restore replaces the jar except on live sessions, which merge
- `page.cookies()` (current URL) / `page.all_cookies()` (all sites in the page's browser context) / `page.set_cookie(name, value, domain, path)` / `page.delete_cookie(name, domain)`
- `BrowserSession::cookies_for_url(url)` / `cookie_header_for(url)` - RFC 6265 scoped, exact values, `Error::InvalidUrl` on ambiguous URLs; `save` writes `0600` atomically
- `page.clear_all_cookies()` / `page.set_cookies_bulk(cookies)` - Bulk operations
- `page.set_extra_headers(headers)` / `page.clear_extra_headers()` - Custom HTTP headers
- `page.capture_requests(RequestCaptureOptions)` - Page-scoped bounded `RequestCapture`: `requests`, `get`, `drain_completed`, `wait_for`, `stats`, `response_body`, `stop`. Never sends `Network.disable` (that drops extra headers)
- `page.enable_request_capture()` / `page.disable_request_capture()` - Enable/disable the Network domain
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
- `elem.click()` / `elem.human_click()` - Click (`ActionCall`)
- `elem.center()` - Get center coordinates
- `elem.type_text(text)` (`ActionCall`) / `elem.focus()` - Input
- `elem.is_visible()` - Check if rendered (returns `Result<bool>`)
- `elem.bounding_box()` - Get position/size (handles rotated elements)
- `elem.get_attribute(name)` - Get attribute
- `elem.tag_name()` / `elem.value()` / `elem.text()` / `elem.outer_html()`
- `elem.is_enabled()` / `elem.is_checked()` - State
- `elem.css(property)` - Computed style
- `elem.scroll_into_view()` - Scroll into viewport

## Key Design Decisions

### CDP Command Filtering
Transport refuses detectable commands in `src/cdp/transport.rs` (`is_blocked`):
`Runtime.enable/disable` (prevents consoleAPICalled detection),
`Debugger.enable/disable`, `HeapProfiler.enable/disable`,
`Profiler.enable/disable`, `Console.enable/disable`. A blocked command returns
`Error::Cdp` naming `filter_cdp`. Launched and connected browsers honour
`StealthConfig.filter_cdp`; `live()` turns it off.

### Transport
A single writer task (`transport/writer.rs`) owns the socket, with a priority
lane for pongs and automatic Fetch replies, so the reader never waits on a
write. Events are broadcast as `Arc<CdpMessage>` (`EVENT_CHANNEL_CAPACITY`
4096); a subscriber that lags must recover (rescan targets, re-read the frame
tree) rather than fail an action that was already dispatched. Pending commands
record their session and fail with Chrome's `-32001` on detach. `Connection`
drops target state on `Target.targetDestroyed`, or on detach once
`Target.getTargetInfo` reports the target gone. Command ids use
1..=999,999,999 and automatic replies 1e9..=i32::MAX, both wrapping.

### X-Client-Data Header
Chromium sends `X-Client-Data` (field-trial variation IDs) on requests to
Google-owned domains; a fresh automation profile has a distinctive set that
Google scores before any page JS runs. Eoka launches with
`--disable-field-trial-config` and strips the header via CDP Fetch
interception auto-continued in the transport (`StealthConfig.strip_x_client_data`).
DevTools silently drops commands with ids above int32. Only targets eoka
attaches are intercepted; out-of-process iframes and workers are not.

### Proxy Authentication
Configured proxy credentials answer only explicit `Proxy` challenges, on both
launched and connected browsers.
`Server`, missing, and unknown sources receive `CancelAuth` without credentials.
Without configured proxy credentials, auth events remain available to the caller.
With `config.proxy` set, Chrome runs with
`--webrtc-ip-handling-policy=disable_non_proxied_udp`.

### Binary Patching
Opt-in (`patch_binary`, default false). Stock Chrome has no automation
markers. When enabled, only ChromeDriver's `$cdc_`/`$wdc_`/`$chromeDriver`
inside non-executable sections are rewritten (ELF, Mach-O, PE headers are
parsed; unparseable binaries are refused). Linux cache:
`~/.cache/eoka/patched-chrome`, verified by manifest and content hash.

### Evasion Shape
Evasions patch interface prototypes through the prelude helpers
`_eoka_patchMethod`/`_eoka_patchGetter` (native name, length, `toString`,
non-constructible, receiver checks) and only where Chrome reports something
wrong; `window.chrome`, plugins, permissions, `webdriver` and connection are
native in current Chrome. Timezone comes only from
`Emulation.setTimezoneOverride`. Never define properties on instances.
Cross-site (out-of-process) iframes and workers get no evasions.

### Launch Flags
User `--disable-features`, `--enable-features`, `--disable-blink-features`,
`--enable-blink-features` and `--blink-settings` in `extra_args` are merged
into eoka's (Chrome keeps only the last copy of a switch).

### Fingerprint Persistence
A durable profile stores its identity in `.eoka-fingerprint.json` (0600). Only
an explicitly configured user agent must match it; the detected Chrome version
updates it. macOS identities claim Apple-silicon renderers and `arm`.

### Text Finding Priority
`src/page/text_candidates.js` is shared by `find_by_text`, `find_all_by_text`
and text locators:
1. Interactive elements: `a, button, input[type="submit"], input[type="button"], [role="button"], [onclick]`
2. Otherwise the innermost static match among `div, span, p, label, h1-h6, li, td, th`
Arguments reach page scripts as JSON, never spliced into source.

### Error Handling
Chrome reports lifecycle conditions only as message text. The shared
classification (session gone, context lost, node gone, not rendered) lives in
`src/error/classify.rs` (`CdpFailure`); use it instead of new string matches.
Frame discovery and shadow retries (`page/frames`), preparation retries
(`action/perform.rs`) and recording support detection still match their own
messages. CDP "box model" errors become
`ElementNotVisible`. `try_*` returns `Ok(false)` for missing, invisible,
moved, covered and stale targets when no input was sent.

## Error Types

```rust
Error::Coded { code: ErrorCode, message } // eoka's own usage/policy/state errors
Error::Action { code: ErrorCode, receipt, source }
Error::ElementNotFound(selector)      // Not in DOM
Error::ElementNotVisible { selector } // In DOM but not rendered
Error::Timeout(message)
Error::InvalidUrl(message)            // Unusable URL; message omits it
Error::InvalidScreenshotOptions(message) // Invalid format/quality/crop combination
Error::ValueMismatch { selector, expected_chars, actual_chars } // never the values
Error::RetryExhausted { attempts, last_error, source }
Error::Cdp { method, code, message }  // Chrome protocol errors, plus remaining internal invariant failures
```

Raise new eoka-side failures with `Error::coded(ErrorCode::X, msg)`, never with a
code prefix inside `Error::cdp_msg`. `ErrorCode` compares equal to its
snake_case string. `Error::is_retryable` drives `with_retry`.

## Evasion Scripts

JavaScript lives in `src/stealth/evasions/scripts/`; public constants and script
assembly remain in `src/stealth/evasions.rs`. `include_str!` embeds these assets
at compile time, so no runtime file reads are needed.

| Script | Purpose |
|--------|---------|
| `NATIVE_TOSTRING_EVASION` | Prelude: patch helpers and native `toString` |
| `WEBDRIVER_EVASION` | navigator.webdriver, only if Chrome exposes it |
| `CHROME_RUNTIME_EVASION` | window.chrome, only if missing |
| `PERMISSIONS_EVASION` | Notification/Permissions consistency, only if inconsistent |
| `PLUGINS_EVASION` | The five PDF plugins, only if the list is empty |
| `NAVIGATOR_PROPS_EVASION` | languages (one cached frozen array), platform, hardware |
| `HEADLESS_EVASION` | PerformanceObserver entry types on old headless builds |
| `BATTERY_EVASION` | navigator.getBattery() |
| `NAVIGATOR_EXTRA_EVASION` | connection, only where legacy headless reports all zeros |
| `WEBGL_EVASION` / `CANVAS_EVASION` / `AUDIO_EVASION` | Gated by `webgl_spoof`/`canvas_spoof`/`audio_spoof` |
| `SPEECH_EVASION` | speechSynthesis.getVoices() |
| `MEDIA_DEVICES_EVASION` | mediaDevices.enumerateDevices() |
| `BLUETOOTH_EVASION` | navigator.bluetooth API |
| `FONTS_EVASION` | document.fonts.check for system fonts (page fonts stay native) |
| `KEYBOARD_EVASION` | navigator.keyboard.getLayoutMap() |

`CDP_EVASION`, `TIMEZONE_EVASION` and `WEBRTC_EVASION` are deprecated empty
strings; `FINGERPRINT_EVASION` is WebGL + canvas + audio.

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
1. Add method to `impl Page` in `src/page.rs` or the matching `src/page/` module
2. Pass values to page scripts as `callFunctionOn` arguments or JSON literals; no DOM mutation
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
- `tokio-tungstenite` / `futures-util` - WebSocket transport (no TLS)
- `libc` - Unix process handling
- `schemars` - optional JSON schemas (`schema` feature)
- `serde`/`serde_json` - serialization
- `aho-corasick` - binary patching (opt-in)
- `memmap2` - memory-mapped file I/O
- `fastrand` - human simulation randomness
- `base64` - screenshot/response encoding
- `thiserror` - error types
- `tracing` - logging

## Exported Types

See the `pub use` block in `src/lib.rs`; it is the source of truth. Highlights:
`Browser`, `Page`, `Element`, `BoundingBox`, `Locator`, `Action`, `ActionCall`,
`TryActionCall`, `ActionOptions`, `ActionReceipt`, `Expectation`, `Destination`,
`InputMode`, `Error`, `ErrorCode`, `Result`, `RequestCapture`,
`RequestCaptureOptions`, `NetworkWatcher`, `BrowserSession`, `SessionCookie`,
`DownloadWatch`, `BoundedFetchRequest`, `FrameResponseCapture`,
`NativeRecording`, `ScreenshotOptions`, `Fingerprint`, `HumanSpeed` and
`StealthConfig`.
