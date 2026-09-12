# Changelog

All notable changes to this project will be documented in this file.

## [0.5.9] - 2026-09-12

### Changed

- Separate geo lookup, response parsing, and language defaults from browser
  lifecycle handling. Preserve endpoint order, fallback behavior, and profile
  identity updates.

## [0.5.8] - 2026-08-29

### Changed

- **`Fingerprint` is now the single source of platform truth.** All
  emulated-machine data lives on the fingerprint and is persisted with durable
  profiles: `voices` (`SpeechVoice` list, platform-correct and stable per
  identity) and `media_devices` (`MediaDevicesSpec` — counts typed to the
  claimed OS, e.g. Linux desktops usually have no webcam). Evasion scripts are
  now dumb templates: placeholder values are filled from one list and must all
  come from the fingerprint (guarded by `test_placeholders_are_filled`).
- Derived platform values are methods, not ad-hoc logic: `ch_architecture()`
  replaces the "Apple M" renderer string-matching hack in client hints, and
  `ui_chrome_height()` gives per-OS taskbar/menu-bar heights for
  `screen.availHeight` (previously a hardcoded Windows 40px).
- Fake media-device IDs are generated from the identity's noise seed in Rust,
  stable per profile and distinct across profiles; the JS no longer derives
  them.
- **Breaking:** persisted identities missing the new fields no longer load —
  profiles created before this change must be deleted (durable-profile
  directories under `~/.local/state/eoka/profiles`).
- **Fixed:** the `Accept-Language` client-hint header is now derived from
  `navigator.languages` (Chrome's q-value ordering) instead of hardcoded
  `en-US,en;q=0.9` — geo-aligned identities no longer leak a header/JS
  language mismatch.
- Deleted dead code: unused `Fetch.continueRequest` / `Fetch.fulfillRequest` /
  `Fetch.failRequest` / `Fetch.disable` session wrappers and their CDP types
  (the transport auto-responder handles interception directly).

### Fixed

- **Timezone split-brain** (verified against stock Chrome): the JS shim made
  `Intl` and `getTimezoneOffset()` claim the fingerprint timezone while
  `Date.toString()`/`getHours()` still reported the host timezone — an
  inconsistent pair any page can compare. Sessions now apply native
  `Emulation.setTimezoneOverride` so the entire `Date`/`Intl` surface agrees.
- **Impossible `prefers-reduced-motion` state**: the `matchMedia` wrapper
  returned `matches: false` for both `reduce` and `no-preference` (one must
  always match), and returned a non-`MediaQueryList` object. The wrapper is
  removed; stock headless/new already reports the correct pair.
- **`window.chrome` didn't match stock Chrome**: eoka injected a `runtime`
  object that plain Chrome doesn't expose to pages (it's extension-only) and
  is now verified against stock Chrome 152: keys are exactly
  `["loadTimes", "csi", "app"]`.
- **`navigator.deviceMemory` could report 16 or 32** — impossible values;
  Chrome caps the report at 8. Buckets are now 4/8.
- **macOS speech voices on non-macOS identities**: `speechSynthesis`
  fallback voices are now platform-aware (macOS/Windows lists; Linux passes
  through, matching stock Linux Chrome's zero voices).
- **Cross-session linkable media devices**: fake `enumerateDevices`
  group/device IDs were one eoka-wide constant; they are now derived from the
  per-fingerprint noise seed.
- **Canvas hook inconsistency**: `toDataURL`/`toBlob` were noised but plain
  `getImageData` reads weren't, so a page comparing both could flag the
  canvas. `getImageData` now applies the identical LSB noise.
- `Intl.DateTimeFormat` no longer mutates the caller's options object.
- **Profile directories were world-readable**: spawned profiles and CLI
  profile clones (containing the cookie store) now get 0700 permissions.

### Added

- **Font identity**: `Fingerprint.fonts` — a platform-typical system font set
  (Windows Segoe UI family, macOS Helvetica family, Linux DejaVu/Liberation
  family, with per-identity variation on Linux). The new `document.fonts.check`
  spoof resolves only claimed fonts plus CSS generic families, so font
  enumeration reports the claimed OS instead of the host. Measurement-based
  font probing (canvas `measureText`) is NOT covered.
- **Keyboard layout**: `Fingerprint::keyboard_map_json()` — US ANSI
  `KeyboardLayoutMap` for Windows/macOS identities (matching their en-US
  locale); Linux passes through untouched (no fabrication where stock Chrome
  exposes the real host layout).
- **`StealthConfig.strip_x_client_data`** (default `true`). Chrome sends an
  `X-Client-Data` request header with field-trial variation IDs to Google-owned
  properties; a fresh automation profile has a distinctive (often empty) set
  that server-side bot scoring reads before any JavaScript runs. Eoka now
  launches Chrome with `--disable-field-trial-config` and strips the header
  from every request via CDP `Fetch` interception (auto-continued in the
  transport reader). Also fixes a latent transport bug: auto-responder CDP
  command IDs above `i64::MAX` are silently dropped by DevTools, which would
  have stalled any request paused by interception (and any `Fetch.authRequired`
  flow).
- **`StealthConfig.geo_align`** (default `false`). Resolves timezone and
  `navigator.languages` from the browser's apparent public IP (ipwho.is, with a
  Cloudflare-trace country fallback) via a one-shot navigation before the first
  real page, keeping `Date`/`Intl`, `Accept-Language`, and the fingerprint
  consistent with IP geolocation instead of a random timezone.

## [0.5.0] - 2026-07-06

A refactor release: smaller, more reusable public API and a modern async
transport. See the migration notes below.

### Changed

#### Breaking

- **`Element` is now owned, not borrowed.** `Element<'a>` → `Element`; it holds a
  cheap clone of its `Page`, so elements are `Send + 'static` — you can store
  them, return them, and use them across `.await`/navigation. `Page` and
  `Session` gained `Clone`.
- **CDP internals are no longer public API.** The ~90 hand-written `cdp::types`
  structs and the typed `Session`/`Connection` methods are now `pub(crate)`, so
  adding a CDP field is no longer a breaking change. The deliberate low-level
  escape hatch remains public: `Session::{session_id, target_id, transport,
  send}`, `Connection::transport`, `Transport`, and `cdp::discover::*`.
- **Cookies use the domain `SessionCookie`.** `Page::cookies() ->
  Vec<SessionCookie>`, `Page::set_cookies_bulk(Vec<SessionCookie>)`, and
  `BrowserSession::new(Vec<SessionCookie>)` (previously exposed the internal cdp
  `Cookie`/`NetworkSetCookie`).
- **Transport constructors are `async`** (`Transport::new`/`new_with_options`/
  `connect`/`connect_with_options`) — the WebSocket connect is awaited.
- **`Error` changes:** `Error::CdpSimple(String)` removed; `Error::Cdp` now has
  `method: Option<String>` and `code: Option<i64>` (use `Error::cdp(...)` or the
  new `Error::cdp_msg(...)`). `Error` is `#[non_exhaustive]` — match with a `_`
  arm. `Error` now implements `Clone`.
- Removed the unused `full_evasion_script` public function.

#### Transport rewrite

- The hand-rolled RFC-6455 WebSocket layer was replaced with
  `tokio-tungstenite` (no TLS); the blocking `std::thread` reader is now an
  async task. Events moved from a lossy 256-slot `mpsc` to a multi-consumer
  `tokio::sync::broadcast` (new `Transport::subscribe()`); `recv_event` no
  longer silently drops events. `transport.rs` shrank 843 → 597 lines.
  New deps: `tokio-tungstenite`, `futures-util` (tokio `net`/`io-util`/`rt`).

### Fixed

- **`find_chrome` now locates non-stable Chrome channels** (Beta/Dev): it
  resolves the real ELF sibling of a channel wrapper script, instead of only
  hardcoding the stable `/opt/google/chrome` path.
- `find_by_text`/`find_all_by_text` prime `DOM.getDocument` before
  `DOM.requestNode` (previously returned 0 on an unpopulated node-id space).

### Internal

- Split `page.rs` (1815 lines) into `page.rs` + `element.rs` + `keyboard.rs`.
- Every public item is now documented (`#![deny(missing_docs)]`).

## [0.4.0] - 2026-07-06

### Fixed

#### Transport
- Reassemble fragmented WebSocket frames (previously dropped, hanging the request)
- A mid-frame read timeout is now fatal instead of silently desyncing the stream
- Kill and reap Chrome on failed launch/handshake and DevTools-URL timeout so a partial launch can't orphan a process
- `Error` Display now surfaces the underlying io cause

#### Stealth
- A single `Fingerprint` now drives the UA, `Sec-CH-UA*` client-hint metadata, `navigator.platform`, WebGL and the injected script — previously hardcoded values contradicted the UA (e.g. `MacIntel` on a Windows UA)
- Added `Function.prototype.toString` native masking; stable `navigator.plugins` reference; deterministic canvas/audio noise
- Removed the `__eoka_pending_requests` page beacon and the page-breaking `Image` naturalHeight hack

#### Binary patcher
- Resolve the real ELF next to a channel wrapper — **fixes Chrome Beta/Dev, which `find_chrome` could not locate at all**
- Stable content-addressed cache (no ~400MB re-copy/re-patch per launch); verification excludes non-rewritten patterns

#### Cookies
- Percent-encode CR/LF and sanitize cookie names (closes a header-injection hole)
- Host-only cookies no longer leak to subdomains (RFC 6265)

#### Page / network capture
- Cache the document node so `find()` no longer invalidates previously-returned element handles
- `goto` waits for load; `wait_for_hidden` handles `display:none`; `\0` escapes as `\x00`
- Retain completed requests, keep the original entry across redirects, non-blocking event emit, O(1) eviction

### Added
- `Session::set_user_agent_full` with client-hint metadata
- `BoxModel::try_center` (fallible center)
- 44 regression/unit tests

### Changed

#### Behavior
- `Element::center` now errors (`ElementNotVisible`) instead of returning `(0.0, 0.0)` and clicking the viewport corner

#### Deprecated
- `BoxModel::center` — use `try_center`

---

## [0.2.1] - 2025-01-26

### Fixed

#### Race Conditions
- `find_by_text_match` and `find_all_by_text` now use unique marker IDs to prevent race conditions between concurrent calls

#### Security
- Added comprehensive JavaScript string escaping (handles `\`, `'`, `"`, `` ` ``, `\n`, `\r`, `${}`)
- `evaluate_in_frame` now uses `Function` constructor instead of `eval()` (CSP-safe)

#### Code Quality
- Extracted `handle_try_result` helper to reduce duplication in try-click methods
- Element inspection methods now use `eval_on_element` helper with focus restoration
- `Element::text()` no longer changes focus as a side effect

#### Bug Fixes
- `bounding_box()` now correctly handles rotated/transformed elements using min/max of quad points
- `debug_screenshot()` now respects `StealthConfig::debug_dir`
- `find_all_by_text` now always cleans up markers, even on errors

### Changed

#### Breaking Changes
- `Element::is_visible()` now returns `Result<bool>` instead of `bool` to distinguish between "not visible" and "network error"
- `PageState` counts (`input_count`, `button_count`, etc.) changed from `i32` to `u32`

#### API Changes
- Added `#[must_use]` attributes to `try_click*`, `exists`, `text_exists`, `is_visible`
- Removed non-functional `find_in_frame` method (use `evaluate_in_frame` instead)

---

## [0.2.0] - 2025-01-26

### Added

#### Text-Based Element Finding
- `find_by_text(text)` - Find element by text content (prioritizes interactive elements)
- `find_by_text_match(text, TextMatch)` - Find with matching strategy (Exact, Contains, StartsWith, EndsWith)
- `find_all_by_text(text)` - Find all elements matching text
- `text_exists(text)` - Check if text exists without error
- `TextMatch` enum for flexible text matching strategies

#### Selector Fallback Chains
- `find_any(&[selectors])` - Try multiple selectors, return first match
- `wait_for_any(&[selectors], timeout)` - Wait for any selector to appear
- `wait_for_any_visible(&[selectors], timeout)` - Wait for any selector to be visible

#### Click Improvements
- `click_by_text(text)` - Click element by visible text
- `human_click_by_text(text)` - Human-like click by text
- `try_click(selector)` - Returns `Ok(false)` instead of error when not found/visible
- `try_click_by_text(text)` - Try-click by text
- `try_human_click(selector)` - Try human-click
- `try_human_click_by_text(text)` - Try human-click by text

#### Form Filling
- `fill(selector, value)` - Clear field and type value
- `human_fill(selector, value)` - Human-like clear and type

#### Wait Helpers
- `wait_for_visible(selector, timeout)` - Wait for element to be rendered and clickable
- `wait_for_text(text, timeout)` - Wait for text to appear
- `wait_for_text_hidden(text, timeout)` - Wait for text to disappear
- `wait_for_url_contains(pattern, timeout)` - Wait for URL pattern
- `wait_for_url_change(timeout)` - Wait for any URL change
- `wait_for_network_idle(idle_ms, timeout)` - Wait for no pending fetch/XHR requests

#### Element Inspection
- `Element::is_visible()` - Check if element has computable box model
- `Element::bounding_box()` - Get element's bounding box
- `Element::get_attribute(name)` - Get attribute value
- `Element::tag_name()` - Get tag name (div, a, button, etc.)
- `Element::is_enabled()` - Check if not disabled
- `Element::is_checked()` - Check checkbox/radio state
- `Element::value()` - Get input value
- `Element::css(property)` - Get computed CSS property
- `Element::scroll_into_view()` - Scroll element into viewport

#### Frame/Iframe Support
- `frames()` - List all frames on page
- `evaluate_in_frame(frame_selector, js)` - Execute JS inside iframe
- `FrameInfo` struct with id, url, name

#### Retry Operations
- `with_retry(attempts, delay_ms, operation)` - Retry flaky operations

#### Debug Helpers
- `debug_screenshot(prefix)` - Save timestamped screenshot
- `debug_state()` - Get `PageState` with element counts
- `PageState` struct with url, title, input/button/link/form counts
- `BoundingBox` struct with x, y, width, height

#### Configuration
- `StealthConfig::debug` - Enable debug mode
- `StealthConfig::debug_dir` - Directory for debug screenshots
- `StealthConfig::debug()` - Preset for debug configuration

### Improved

#### Better Error Messages
- `ElementNotVisible` - "exists in DOM but not rendered" (replaces cryptic CDP errors)
- `ElementNotInteractive` - Element cannot be interacted with
- `RetryExhausted` - After N retry attempts
- `FrameNotFound` - Frame/iframe not found
- `Error::is_not_visible()` - Check if error is visibility-related
- `Error::clarify(selector)` - Convert CDP errors to friendly messages

#### Text Matching
- `find_by_text()` now prioritizes interactive elements (a, button, input) over static elements
- Two-pass search: interactive elements first, then static elements

#### Try-Click Methods
- Now catch both `ElementNotFound` AND CDP box model errors
- Return `Ok(false)` for invisible elements instead of error

### Documentation
- Comprehensive README with real-world login example
- Full API reference for all new methods
- Recipes section for common patterns
- Error handling documentation

---

## [0.1.0] - 2025-01-26

### Added

- Initial release of eoka stealth browser automation library
- Custom CDP transport with built-in command filtering (blocks detectable commands like `Runtime.enable`)
- 15 JavaScript evasion scripts:
  - WebDriver property interception via Proxy
  - Navigator plugins/mimeTypes spoofing
  - Chrome runtime properties
  - Permissions API consistency fixes
  - Battery API on Navigator.prototype
  - WebGL vendor/renderer masking
  - Canvas noise injection
  - Audio fingerprint protection
  - iframe contentWindow fixes
  - Broken image dimension hiding
  - CDP property cleanup
  - WebRTC IP leak prevention
  - Speech synthesis voices spoofing
  - Media devices enumeration
  - Bluetooth API presence
- Human simulation with Bezier curve mouse movements and variable typing delays
- Chrome binary patching to remove automation strings (`$cdc_`, `webdriver`)
- Fingerprint generation (realistic User-Agent, screen dimensions)
- HTTP request capture via CDP Network domain with event streaming (`NetworkWatcher`)
- Screenshot capture with optional annotations (`annotate` feature)
- Session/cookie export for persistence
- GitHub Actions CI workflow (test, clippy, fmt, docs)
- 15 integration tests for CDP commands (browser launch, navigation, screenshots, etc.)

### Features

- `default` - Core functionality
- `annotate` - Screenshot annotations with numbered boxes on interactive elements

### Detection Test Results

- bot.sannysoft.com: All tests pass (including WebDriver New)
- arh.antoinevastel.com/bots/areyouheadless: Not detected
- bot-detector.rebrowser.net: 6/6 tests pass
- browserleaks.com: Clean fingerprint
