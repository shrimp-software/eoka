# eoka

[![crates.io](https://img.shields.io/crates/v/eoka.svg)](https://crates.io/crates/eoka)
[![docs.rs](https://docs.rs/eoka/badge.svg)](https://docs.rs/eoka)
[![CI](https://github.com/shrimp-software/eoka/actions/workflows/ci.yml/badge.svg)](https://github.com/shrimp-software/eoka/actions/workflows/ci.yml)

Stealth browser automation in Rust. Passes bot detection without the bloat.

## Requirements

Chrome or Chromium installed. eoka launches and controls it via CDP.

## Install

```toml
[dependencies]
eoka = "0.5"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

## Quick Start

```rust
use eoka::{Browser, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let browser = Browser::launch().await?;
    let page = browser.new_page("https://example.com").await?;

    page.human_click("#button").await?;
    page.human_type("#input", "hello").await?;

    let png = page.screenshot().await?;
    std::fs::write("screenshot.png", png)?;

    browser.close().await?;
    Ok(())
}
```

## Login Flow Example

```rust
use eoka::{Browser, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let browser = Browser::launch().await?;
    let page = browser.new_page("https://example.com/login").await?;

    page.try_click_by_text("Accept Cookies").await?;
    page.human_click_by_text("Sign In").await?;
    page.wait_for_visible("#email", 10_000).await?;
    page.human_fill("#email", "user@example.com").await?;
    page.human_fill("#password", "secret123").await?;
    page.human_click_by_text("Log In").await?;
    page.wait_for_text("Welcome back", 15_000).await?;

    browser.close().await?;
    Ok(())
}
```

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

```rust
page.click("#button").await?;                   // instant
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

```rust
page.press_key("Enter").await?;
page.press_key("Ctrl+A").await?;
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

```rust
page.with_retry(3, 500, || async {
    page.human_click("#flaky").await
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

## Detection Results

Patches Chrome binary, injects 15 evasion scripts, blocks detectable CDP commands at the transport layer, simulates human input with Bezier curves.

- **Passes**: sannysoft, rebrowser bot detector (6/6), areyouheadless, browserleaks
- **Partial**: creepjs (33% trust score)

## How it Works

~9K lines of Rust (about 1.3K of that is tests). No chromiumoxide, no puppeteer-extra. Hand-written CDP types for the ~30 commands actually needed. Minimal dependency graph.

```
src/
├── cdp/           # raw websocket transport, command filtering
├── stealth/       # evasions, binary patcher, human simulation
├── browser.rs     # chrome launcher
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
