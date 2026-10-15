# eoka

[![crates.io](https://img.shields.io/crates/v/eoka.svg)](https://crates.io/crates/eoka)
[![docs.rs](https://docs.rs/eoka/badge.svg)](https://docs.rs/eoka)
[![CI](https://github.com/shrimp-software/eoka/actions/workflows/ci.yml/badge.svg)](https://github.com/shrimp-software/eoka/actions/workflows/ci.yml)

Stealth browser automation in Rust. Passes bot detection without the bloat.

## Requirements

Rust 1.85 or newer, and Chrome or Chromium installed. eoka launches and controls
the browser via CDP.

## Install

```toml
[dependencies]
eoka = "0.7"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

## Quick Start

```rust
use eoka::{ActionOptions, Browser, Expectation, LocatorSpec, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let browser = Browser::launch().await?;
    let page = browser.new_page("data:text/html,<input id=name><button onclick=\"document.body.dataset.done=1\">Save</button>").await?;
    page.fill("#name", "hello").await?;
    let receipt = page.locator("button").click(ActionOptions::expecting(
        Expectation::Visible { target: LocatorSpec::css("body[data-done]") },
    )).await?;
    assert_eq!(receipt.outcome, eoka::ActionOutcome::VerifiedComplete);

    let png = page.screenshot().await?;
    std::fs::write("screenshot.png", png)?;

    browser.close().await?;
    Ok(())
}
```

## Login Flow Example

```rust
use eoka::{ActionOptions, Browser, Expectation, LocatorSpec, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let browser = Browser::launch().await?;
    let page = browser.new_page("https://example.com/login").await?;

    page.locator_text("Sign In").human_click(ActionOptions::expecting(
        Expectation::Visible { target: LocatorSpec::css("#email") },
    )).await?;
    page.wait_for_visible("#email", 10_000).await?;
    page.human_fill("#email", "user@example.com").await?;
    page.human_fill("#password", "secret123").await?;
    page.locator_text("Log In").human_click(ActionOptions::expecting(
        Expectation::Text { target: LocatorSpec::css("#welcome"), value: "Welcome back".into(), contains: true },
    )).await?;

    browser.close().await?;
    Ok(())
}
```

## Dependable actions in 0.6

Actions use strict locators, visibility/enabled checks, stable hit-tested pointer
positions and one deadline. Click, typing, key and drag actions require an
explicit expected result. Successful actions and `Error::Action` failures carry
receipts distinguishing not dispatched, unknown outcome and verified completion.
No action is replayed after dispatch. [Migration and complete contract](MIGRATION-0.6.md).
Durable operation IDs and download recovery are provided by eoka-tools.

For an agent exploring an unfamiliar page, `page.explore(action, options)` uses
the same actionability checks, deadline and input cleanup while allowing actions
without a predicted result. A dispatched click returns `Ok(receipt)` with
`outcome_unknown` unless an explicit expectation proves completion. Intrinsic
checks, such as verifying a filled value, still run. Always inspect the receipt;
`Ok` alone does not establish that the website completed the task. `Page::act`
and existing convenience methods retain their strict verification contract.

## API

### Browser

```rust
let browser = Browser::launch().await?;
let browser = Browser::launch_visible().await?;
let browser = Browser::launch_debug().await?;
let browser = Browser::launch_with(|config| {
    config.proxy = Some("http://127.0.0.1:8080".into());
}).await?;
let browser = Browser::launch_with_config(config).await?;
let page = browser.new_page("https://example.com").await?;
let tabs = browser.tabs().await?;
browser.activate_tab(id).await?;
browser.close_tab(id).await?;
browser.close().await?;
```

### Viewport and device scale

```rust
page.set_viewport(640, 360, 2.0).await?;
let png = page.screenshot().await?;
page.reset_viewport().await?;
```

Dimensions are CSS pixels; 640×360 at device scale factor 2 produces a
1280×720 PNG. Width and height must be positive and fit a signed 32-bit CDP
dimension. Scale must be positive and finite.

Non-live sessions apply `StealthConfig.viewport_width`, `viewport_height` and
`device_scale_factor` at setup. Live attachment preserves existing metrics until
explicitly changed. Overrides persist across navigation. Reset clears emulation
and restores native window content dimensions/device scale; it does not resize
the window or restore the configured launch override.

Fractional host display scaling can quantize effective CSS dimensions and DPR.
Use measured DOM layout as the authority; Eoka does not mask those native values.
Fractional capture boundaries follow Chrome's native rasterization, including
edge rounding/artifacts. For pixel-aligned exports, align dimensions and DPR with
the host pixel grid and verify the captured frame.
Native capture/reset can refresh Chrome's hidden-tab `outerWidth`/`outerHeight`
readings without changing actual browser window bounds; Eoka does not mask these
native values.
Viewport changes and captures are coordinated across Eoka handles connected
through the same browser endpoint in this process. Independent CDP clients and
other processes are outside that coordination; explicitly set the desired
viewport before capturing a tab they have emulated.

### Typed screenshots

```rust
use eoka::{ScreenshotClip, ScreenshotFormat, ScreenshotOptions};

let webp = page.screenshot_with_options(&ScreenshotOptions {
    format: ScreenshotFormat::Webp,
    quality: Some(90),
    clip: Some(ScreenshotClip { x: 20.0, y: 100.0, width: 320.0, height: 180.0 }),
    full_page: false,
}).await?;
let full_png = page.screenshot_with_options(&ScreenshotOptions {
    full_page: true,
    ..Default::default()
}).await?;
```

`screenshot()` remains the viewport PNG convenience and `screenshot_jpeg(80)`
captures viewport JPEG. Typed options also support WebP. JPEG/WebP quality must
be 0–100; PNG rejects quality. Crop coordinates are document-relative CSS pixels,
including portions outside the visible viewport, with finite nonnegative positions
and finite positive dimensions. Crop and full-page are mutually exclusive.
Invalid options return `Error::InvalidScreenshotOptions` before contacting Chrome,
including out-of-range quality supplied to `screenshot_jpeg`.

Viewport DPR is the only render scale: a 320×180 crop at DPR 2 yields 640×360
pixels. Full-page capture uses Chrome's CSS content bounds without resizing the
viewport; capture preserves viewport metrics and scroll position. Large captures
remain subject to Chrome's renderer/image-size and available-memory limits and
the 64 MiB CDP message/frame budget, including base64 and JSON overhead. They
can fail with a CDP or transport error. For deterministic exports, use a fixed
viewport or crop rather than a full page whose layout dimensions may change.

### Native recording

```rust
use eoka::NativeRecordingOptions;
use std::io::Write;

let recording = page.start_recording(NativeRecordingOptions {
    max_width: Some(1280),
    max_height: Some(720),
    fps: Some(30.0),
}).await?;
// Drive the page while recording, then explicitly finalize.
recording.stop().await?;
let mut file = std::fs::OpenOptions::new()
    .write(true).create_new(true).open("capture.mp4")?;
loop {
    let chunk = recording.read_chunk().await?;
    file.write_all(&chunk.data)?;
    if chunk.eof { break; }
}
recording.close().await?;
file.flush()?;
```

The experimental native Chrome recorder requires a ready, active page and allows
one recorder per target, shared across Eoka handles. Unsupported commands or native
encoders fail explicitly; there is no alternate recorder or FFmpeg runtime
requirement. Output is MP4 with a Chrome-selected codec, not guaranteed H.264.
Audio is not promised. Maximum dimensions must be positive; FPS must be an integer
from 1 through 120 and is a ceiling, not deterministic fixed-FPS playback.

Chrome for Testing 155.0.8059.39 can emit zero MP4 sample durations and duplicate
decoding timestamps. This also occurs through direct CDP without Eoka. The core
preserves Chrome's original stream; it does not rewrite timestamps or transcode.
A decodable file is not proof of strictly increasing timestamps. Native recording
remains experimental: consumers must inspect packet timestamps and decoder
diagnostics. The paired Node sidecar separately applies bounded MP4 timing
corrections without transcoding before publication and qualifies the resulting
timestamps and diagnostics. This is not an upstream Chrome fix or a guarantee
about the core's raw stream.

`stop()` enters the output stream; `read_chunk()` returns at most 64 KiB decoded
bytes with explicit EOF, and `close()` releases the stream. `cancel()` discards an
unfinished recording with bounded cleanup; Drop attempts best-effort cleanup when
an async runtime is available. `wait_for_failure()` observes target/session/transport
loss without waiting behind recording commands.
`RecordingError` distinguishes unsupported/precondition/state failures and uncertain
cleanup, which must not be treated as permission to restart. Cleanup acknowledgement
is bounded; if it expires, owned cleanup remains scheduled and the target reservation
is retained rather than reporting that resources were released. The core does not
own output-file publication: callers must handle streaming/write failures and cleanup;
the Node sidecar additionally stages and atomically publishes without overwriting,
and its `Recording.stop()` returns only a completed artifact.

Read chunks sequentially: overlapping `read_chunk()` calls fail with
`RecordingError::InvalidState` without consuming another chunk or cancelling the
accepted read. Cancelling a read before its chunk is delivered fails the recording
and schedules cleanup, rather than allowing a silently truncated output stream.

Connection teardown and borrowed-browser close clean up only recordings owned by
that connection, not another connection's recorder on the same target. Explicit
target close cancels the target's recording. Teardown cleanup survives cancellation
of its waiting caller.

### Finding Elements

```rust
page.find("#button").await?;                    // CSS selector
page.find_all(".item").await?;                  // all matches
page.find_by_text("Sign In").await?;            // by visible text
page.find_any(&["#email", "[name='email']"]).await?; // first match
page.exists("#popup").await;                    // bool
page.text_exists("Error").await;                // bool
```

### Clicking

Configure a fresh expectation for the operation before using convenience methods:

```rust
let page = page.with_action_options(ActionOptions::expecting(
    Expectation::Text { target: LocatorSpec::css("#status"), value: "Done".into(), contains: false },
));
page.click("#button").await?;                   // actionability + verification
page.human_click("#button").await?;             // with mouse movement
page.click_by_text("Submit").await?;
page.human_click_by_text("Submit").await?;
page.try_click("#optional").await?;             // Ok(false) if missing
page.try_click_by_text("Accept").await?;

// Native held pointer input for drags. Call release_all_inputs before close
// if an operation is interrupted.
page.mouse_move(100.0, 100.0).await?;
page.mouse_down(100.0, 100.0, eoka::MouseButton::Left).await?;
page.mouse_move(300.0, 100.0).await?;
page.mouse_up(300.0, 100.0, eoka::MouseButton::Left).await?;
```

### Typing

```rust
page.fill("#email", "user@example.com").await?;     // clear + type + verify
page.fill("#volume", "50").await?;                    // range: validates, sets, input/change
page.human_fill("#email", "user@example.com").await?; // human-like
page.type_into("#search", "query").await?;           // append (no clear)
page.human_type("#search", "query").await?;
```

### Waiting

```rust
page.wait_for("#results", 10_000).await?;           // in DOM
page.wait_for_visible("#email", 10_000).await?;     // visible + clickable
page.wait_for_hidden(".loading", 5_000).await?;
page.wait_for_any(&["#ok", ".error"], 10_000).await?;
page.wait_for_text("Success", 10_000).await?;
page.wait_for_url_contains("dashboard", 10_000).await?;
page.wait_for_url_change(10_000).await?;
page.wait_for_network_idle(500, 30_000).await?;     // XHR/fetch idle
```

Network idle uses a private CDP session installed before a new page navigates.
It waits for the top document to load and for its fetch/XHR requests, including
response bodies, to remain idle for the requested interval. It includes requests
already running when the wait starts and is independent of request capture.
Workers, subframes, and WebSockets are outside this scope. A previously untracked
tab attached with `attach_page` needs a full navigation or reload before this
wait can succeed; changing only the URL fragment is insufficient. Disconnection
or tracking failure returns an error, and the timeout bounds the entire wait.

### Elements

```rust
let elem = page.find("#btn").await?;
elem.click().await?;
elem.is_visible().await?;           // Result<bool>
elem.bounding_box().await;          // Option<BoundingBox>
elem.get_attribute("href").await?;  // Option<String>
elem.tag_name().await?;
elem.value().await?;
elem.text().await?;
elem.is_enabled().await?;
elem.is_checked().await?;
elem.css("color").await?;
elem.scroll_into_view().await?;
```

### Keyboard, Hover, Select, Upload

Printable `press_key` / `key_down` events carry native text: `"o"` inserts `o`,
`"O"` inserts `O`, and `"Shift+o"` inserts `O`. Shift uses US-keyboard ASCII
mappings; single Unicode characters are literal. Ctrl/Alt/Meta shortcuts, key-up
and input cleanup do not insert text. Browser focus, read-only fields and page
`preventDefault()` handlers still determine whether an edit occurs. For whole
strings or composed text, use `type_text`.

```rust
page.press_key("Enter").await?;
page.dispatch_key("Ctrl+A").await?; // raw primitive for manual input sequences
page.key_down("Shift").await?; // held across calls, with browser-native CDP input
page.key_down("ArrowRight").await?;
page.key_up("ArrowRight").await?;
page.key_up("Shift").await?;
page.release_all_inputs().await?; // cleanup interrupted held input
page.select_all().await?;
page.copy().await?;
page.paste().await?;

page.hover("#menu").await?;
page.human_hover("#menu").await?;

page.select("#country", "US").await?;
page.select_by_text("#country", "United States").await?;

page.upload_file("input[type='file']", "/path/to/file.pdf").await?;
page.upload_files("input[type='file']", &["/a.pdf", "/b.pdf"]).await?;
```

### JavaScript & Frames

```rust
let count: i32 = page.evaluate("document.querySelectorAll('li').length").await?;
page.execute("window.scrollTo(0, 1000)").await?;
let title: String = page.evaluate_in_frame("iframe#widget", "document.title").await?;

for frame in page.frames().await? {
    let title: String = page.evaluate_in_frame_id(&frame.id, "document.title").await?;
    let ancestors = page.frame_ancestor_ids(&frame.id).await?;
}
```

`frames()` includes nested out-of-process iframes belonging to this page.
Both evaluation methods use CDP isolated worlds without enabling `Runtime`; they
can access cross-origin frame DOM, but not page-owned JavaScript globals.
`evaluate_in_frame` requires a CSS selector matching exactly one frame element in
the top document; use IDs for nested frames. Bare URLs and numeric indices are
not selectors. Ancestors are ordered from
the immediate parent to the root, excluding the queried frame. These are
snapshot APIs, not stable navigation identities; reacquire IDs after navigation.
Explicit JavaScript `null` decodes as JSON null (or `None`); `undefined` still errors.

`page.frame_element_content_quad(frame_id, selector, index)` returns eight
coordinates in content-corner order, preserving reflections through open and
closed shadow-slot ancestry. Coordinates are relative to the owning CDP
session's viewport, **not necessarily the root page or local iframe**. Selectors
are limited to 4096 bytes and 64 matches; the index must be below 64. Missing,
detached, fragmented and degenerate boxes fail. This is geometry, not a
visibility/hit-test guarantee. Temporary remote objects are released on success;
errors/cancellation schedule bounded best-effort cleanup.

`page.frame_point_to_viewport(frame_id, x, y)` maps frame-local CSS points to the
root viewport through nested/OOPIF borders, padding, scrolling and positive scaling.
Hidden, reflected, rotated or out-of-viewport geometry is rejected, including
closed shadow slots. Wait for rendering after layout changes; this is not a hit test.

For input, `page.frame_point_for_input(frame_id, x, y)` also requires each parent
document to hit the owning iframe at the mapped point. Overlays (including other
iframes) and inaccessible shadow-root hit paths are rejected. Validate the leaf
element separately and use the returned point unchanged; these are snapshot-time
checks, not protection against subsequent page mutation.

### Horizontal dragging and cleanup

`page.human_drag(selector, dx)`, `element.human_drag_by(dx)` and
`Human::drag_by(x, y, dx)` drag with overshoot and settling.
`Human::drag_horizontal_by(x, y, dx)` keeps Y fixed and X monotonic.
Drags preserve other held buttons and reject an already-held left button.
Retain the helper across cancellation to confirm release before further input:

```rust
let human = page.human();
let result = tokio::time::timeout(
    std::time::Duration::from_secs(5),
    human.drag_horizontal_by(120.0, 120.0, 150.0),
).await;
human.finish_drag_cleanup().await?;
result??;
```

Release has a separate three-second timeout. Cancelled cleanup waits can be resumed;
dropping the helper only schedules best-effort release. Cleanup failures require inspection.

### Frame response capture

`page.capture_frame_responses(frame_id, options)` captures responses for exactly
one frame, surviving its removal. Use `snapshot()` for retained records and
`stop().await` for cleanup and loss/error counters; Drop does not wait.
Bodies and headers are opt-in. Defaults: 128 records, 64 KiB/body, 1 MiB total.
Limits bound retention, not transient CDP payloads; Debug omits body/header values.
Fetch headers do not prove which cookies were sent on the wire.

### Low-level request ownership

`eoka::cdp::Transport` provides an opt-in request-stage Fetch route per session:

- `install_request_route(session, bounded_sender, dropped_counter)` claims
  ownership; a duplicate owner is rejected. Delivered `RequestPause` values
  belong to the consumer and must be resolved once, including on shutdown.
- `set_request_interception(session, Some(patterns))` configures interception.
  `None` restores the base policy. Header stripping/proxy authentication may
  require wildcard request interception; consumers then filter URLs themselves.
- `remove_request_route(session)` removes **future** ownership only. Unowned,
  full-queue and closed-queue events are auto-continued; queue failures increment
  the supplied counter. Already-delivered pauses still need resolution.

`RequestPause::continue_request()` preserves the header-stripping policy. For
cancellable work, use `continue_request_with_dispatch()` or
`Transport::send_to_session_with_dispatch()` with a fresh
`eoka::cdp::transport::CommandDispatch` token. `may_have_been_sent()` is
irreversible uncertainty, **not** acknowledgment: never replay a possibly sent
resolution after an error/timeout. A false observation permits handoff only
after the originating future has terminated or been dropped. Tokens cannot be
reused. Cancellation removes the local pending waiter, not the remote command.
Raw pause payloads can contain sensitive request metadata; avoid logging them.

### Page Info & Debug

```rust
page.url().await?;
page.title().await?;
page.content().await?;              // full HTML
page.text().await?;                 // visible text
page.screenshot().await?;           // PNG bytes
page.screenshot_jpeg(80).await?;    // JPEG at quality 80
page.screenshot_with_options(&options).await?; // typed format/crop/full-page bytes
page.debug_state().await?;          // PageState with element counts
page.debug_screenshot("step1").await?; // timestamped screenshot
```

### Multi-Tab

```rust
let page1 = browser.new_page("https://a.com").await?;
let page2 = browser.new_page("https://b.com").await?;
browser.activate_tab(page1.target_id()).await?;
browser.close_tab(page2.target_id()).await?;
```

### Browser ownership and debugging ports

Explicit debugging ports are preserved; live launches default to a nonzero port.
`close()` gives owned browsers up to five seconds to flush state before termination.
Attached browsers are only disconnected, regardless of live-mode configuration.

### Connect to an existing Chrome

Attach to a browser you already launched with `--remote-debugging-port` instead of
spawning one. Defaults to `StealthConfig::live()`, so the user's tabs are left
untouched (no evasion injection).

```rust
// Start Chrome yourself: chrome --remote-debugging-port=9222
let browser = Browser::connect_port(9222).await?;           // HTTP discovery
// or, with the ws:// URL from http://localhost:9222/json/version:
let browser = Browser::connect("ws://127.0.0.1:9222/devtools/browser/...").await?;

for tab in browser.tabs().await? {
    println!("{} — {}", tab.title, tab.url);
}
let page = browser.attach_page(&target_id).await?;
browser.disconnect().await?;                                // leaves Chrome running
```

### Navigation

```rust
page.goto("https://example.com").await?;
page.goto_with_referrer("https://example.com", "https://google.com").await?;
page.goto_with_headers("https://example.com", headers).await?;
page.reload().await?;
page.back().await?;
page.forward().await?;
```

`goto_with_headers` clears extra headers to an empty map after navigation, error,
or cancellation; it does not restore earlier persistent headers. Navigation and
header changes through handles for the same target are serialized. After
cancellation, cleanup runs for up to five seconds and subsequent page commands
wait for it. A cleanup failure remains an error until `clear_extra_headers()`
successfully retries restoration, including on the original attachment session.
Raw transport commands bypass this coordination.

### Network & Cookies

```rust
let cookies = page.cookies().await?;
page.set_cookie("name", "value", Some("example.com"), None).await?;
page.delete_cookie("name", None).await?;
page.clear_all_cookies().await?;

page.enable_request_capture().await?;       // start capturing XHR/fetch
let body = page.get_response_body(id).await?;
page.disable_request_capture().await?;
```

For bounded text inspection with the page's credentials:

```rust
let request = eoka::BoundedFetchRequest::get("/api/me");
let response = page.fetch_bounded(&request).await?;
assert!(response.bytes_read <= 8192);
```

`max_bytes` accepts zero (headers only) through `MAX_TEXT_FETCH_BYTES` (16 MiB).
Known binary MIME bodies are omitted. Reading cancels at the limit and reports
conservative truncation; incomplete trailing UTF-8 is omitted. This bounds retained
text, not Chrome's native buffers. The original `Page::fetch` API is unchanged.

For native files, `browser.watch_downloads(&staging_directory)` exclusively configures
`allowAndName` and subscribes before the caller triggers input. `DownloadWatch`
exposes beginning/progress events, cancellation, popup ancestry, and explicit
`stop()` cleanup. Only owned browsers are supported. Target discovery is enabled
for the watch and disabled on restoration; this assumes no competing owner of
browser download or target-discovery settings. Event loss/disconnection fails;
failed restoration prevents further watchers on that browser. Drop schedules
bounded restoration but cannot confirm it to the caller.

`page.navigate_for_download(url, referrer)` accepts native attachment navigation;
a returned navigation result alone never proves a file downloaded. Wait for native
completion and verify the regular file under its GUID. Untrusted suggested names
must not determine filesystem paths. Destination handling, jobs and no-overwrite
publication live in the sister `eoka-tools` repository. Core regression:
`cargo test --lib download::`.

### Configuration & Dialogs

```rust
page.set_bypass_csp(true).await?;           // disable CSP
page.set_javascript_enabled(false).await?;  // disable JS entirely (before navigation)
page.set_user_agent("custom UA").await?;
page.ignore_cert_errors(true).await?;
page.accept_dialog(None).await?;            // accept alert/confirm
page.dismiss_dialog().await?;               // dismiss dialog
```

`page.ignore_cert_errors` takes effect only for navigations issued after the
call — too late for `new_page(url)`'s own initial navigate. For a bad cert on
the very first load (e.g. an `.onion` service redirecting `http://` →
untrusted `https://`), set `StealthConfig.ignore_cert_errors = true` instead,
or use `new_blank_page()` + `ignore_cert_errors(true)` + `goto()`.

### Retry

Use a locator with one deadline for actions. `with_retry` never retries an
`Error::Action`, including an uncertain timeout. It remains available for
caller-chosen read-only operations.

```rust
let receipt = page.locator("#save").click(ActionOptions {
    timeout_ms: 5_000,
    ..ActionOptions::expecting(Expectation::Visible { target: LocatorSpec::css("#confirmation") })
}).await?;
```

## Config

Most code does not need to construct a config. `Browser::launch()` starts with
stealth defaults in headless mode. `Browser::launch_visible()` and
`Browser::launch_debug()` are explicit opt-ins for local workflows that need a
window.

For small tweaks, mutate the default config inline:

```rust
let browser = Browser::launch_with(|config| {
    config.headless = false;
    config.proxy = Some("http://127.0.0.1:8080".into());
}).await?;
```

Two server-side reputation signals are handled automatically for spawned
stealth browsers (see `StealthConfig`):

- `strip_x_client_data: true` (default) — Chrome's `X-Client-Data` request
  header (field-trial variation IDs) is stripped via CDP Fetch interception on
  every page, and `--disable-field-trial-config` is passed at launch.
- `geo_align: false` — opt-in. Aligns timezone and `navigator.languages` with
  the browser's apparent public IP via a one-shot lookup before the first real
  navigation. Ignored when `timezone` is set explicitly.

### Managed launch and ownership

`chrome_path` selects an absolute, readable executable. A missing, non-executable,
or relative explicit path fails without discovering another browser. Linux
patching resolves an installed wrapper to its associated ELF binary, never to an
unrelated browser. Patching copies that binary and preserves its companion
libraries/resources; the source installation is not modified.

`runtime_dir` selects an absolute writable parent directory. Each owned launch
creates a unique child there (0700 on Unix) containing its ephemeral profile,
patch artifacts, and Chrome temporary files. `TMPDIR`, `TMP`, and `TEMP` are set
only on child commands, never on the calling process. An explicit runtime root
does not depend on the default temporary directory. The caller's root and any
explicit `user_data_dir` are preserved, including existing profile permissions.

On Unix, patch-cache directories must belong to the effective user and grant no
group/other permissions (new directories use 0700). Their canonical ancestors
must belong to that user or root; group/other-writable ancestors require sticky
protection, as with `/tmp`. Unsafe existing directories or links are rejected
without changing permissions. Choose a trusted `runtime_dir` or temporary root.
macOS bundle links are checked after relocation and before cached reuse so they
cannot reach outside the owned copy.

```rust
let browser = Browser::launch_with(|config| {
    config.chrome_path = Some("/opt/chrome-for-testing/chrome".into());
    config.runtime_dir = Some("/var/lib/my-app/browser-runtime".into());
    config.sandbox = true;
}).await?;
assert!(browser.owns_process());
println!("{}", browser.version().await?); // actual CDP browser product
browser.close().await?;
```

`sandbox: false` retains existing callers' unsandboxed launch behavior.
Set `sandbox: true` to enable Chrome's sandbox; an unsupported environment
returns the launch error without retrying unsandboxed. Sandbox-control switches
in `extra_args` are rejected: use the boolean rather than conflicting arguments.
Existing-profile reuse cannot apply new launch policy, so an active profile
rejects explicit executable/runtime paths or enabled sandboxing. Legacy default
reuse remains borrowed and does not modify existing tabs.

`Browser::close` terminates owned Chrome and reports runtime cleanup failures.
`Browser::disconnect` does not send `Browser.close`, but still terminates an owned
process; it preserves Chrome only for borrowed handles. Drop and startup
cancellation perform best-effort owned cleanup, logging failures. Page handles
do not prevent a dropped owned Browser from terminating its process. Borrowed
browser processes and caller profiles are never deleted by this cleanup.

The eoka-tools sidecar's caller-loss cleanup relies on stdin EOF/broken-pipe
detection while the sidecar is still running. Direct SIGKILL of the sidecar or a
process embedding this crate cannot execute destructors: it cannot guarantee
Chrome or runtime cleanup. These are not portable parent-death OS guarantees.

For reusable presets or advanced setup, build a `StealthConfig` directly:

```rust
let config = StealthConfig {
    headless: false,
    patch_binary: true,
    human_mouse: true,
    human_typing: true,
    debug: true,
    ..Default::default()
};

let browser = Browser::launch_with_config(config).await?;

// Presets for advanced config composition
StealthConfig::visible()   // headless: false
StealthConfig::debug()     // headless: false, debug: true
```

## Development checks

```bash
cargo fmt --all --check
cargo clippy --all-features --all-targets --locked -- -D warnings
cargo test --all-features --locked
cargo build --examples --locked
cargo +1.85.0 check --lib --all-features --locked
xvfb-run -a cargo test --all-features --locked --no-fail-fast --tests -- --ignored --test-threads=1
```

The last command requires Chrome and Xvfb and runs every ignored browser test.
CI pins Chrome for Testing 155.0.8059.39 and retains the browser-test log. These
local HTTP fixtures check browser behavior; they do not establish detector bypass.

## Detection Results

Patches Chrome binary, injects 15 evasion scripts, blocks detectable CDP commands at the transport layer, simulates human input with Bezier curves.

- **Passes**: sannysoft, rebrowser bot detector (6/6), areyouheadless, browserleaks
- **Partial**: creepjs (33% trust score)

## How it Works

Eoka uses hand-written CDP schemas and a small dependency graph. Transport,
interception policy, Chrome process management, and session commands have
separate modules. JavaScript evasions are embedded from individual `.js` assets
at compile time.

```
src/
├── cdp/           # connections, transport and domain-specific schemas
├── stealth/       # script assets, fingerprints, binary patcher, human simulation
├── browser.rs     # browser API and session setup
├── browser/       # launch preparation, profile ownership and geo alignment
├── page.rs        # page api
└── session.rs     # cookie export
```

The key insight: most detection comes from CDP commands leaking (`Runtime.enable` fires `consoleAPICalled` events that pages can detect). eoka blocks those at the transport layer and defines navigator properties on the prototype instead of the instance.

## Ecosystem

| Crate | Description |
|-------|-------------|
| [eoka-agent](https://crates.io/crates/eoka-agent) | AI agent layer with MCP server |
| [eoka-runner](https://crates.io/crates/eoka-runner) | Config-based automation (YAML flows) |

## License

MIT
