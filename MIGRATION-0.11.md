# Migrating to eoka 0.11

0.11 replaces message-prefix error codes with typed ones, makes binary
patching opt-in, and changes a few low-level types. Most applications only need
the first section.

## Error codes are typed

Errors eoka raises itself (usage, policy and state errors) are now
`Error::Coded { code: ErrorCode, message }`. Before, they were `Error::Cdp`
with the code at the start of the message.

```rust,ignore
// 0.10
Err(Error::Cdp { message, .. }) if message.starts_with("download_busy:") => retry_later(),

// 0.11
Err(error) if error.code() == Some(ErrorCode::DownloadBusy) => retry_later(),
```

`Error::Action { code, .. }` holds an `ErrorCode`, not a `String`. Comparisons
with the string form keep compiling:

```rust,ignore
matches!(&error, Error::Action { code, .. } if code == "history_unavailable")
```

Code that stored or formatted the code as a `String` can use
`code.as_str()` or `code.to_string()`. `ErrorCode` serializes as its snake_case
string. Codes that used to be reported as `action_failed` now come back as
themselves: `invalid_timeout`, `invalid_expectation`, `actionability_changed`,
`ambiguous_destination`, `target_stale`, `navigation_aborted`,
`navigation_failed` and `value_mismatch`.

New helpers:

- `Error::code()` returns the code of a `Coded` or `Action` error.
- `Error::is_retryable()` says whether repeating the same call can succeed.
  `Page::with_retry` uses it: it no longer retries invalid URLs or options,
  and it does retry action errors where no input was sent.
- `RecordingError` converts into `Error`, so `?` works across both.

Other error changes:

- `Error::ValueMismatch` now has `expected_chars` and `actual_chars` instead
  of the values themselves, which could be passwords. Fill returns it when a
  control settles on a different value (masks, `maxlength`, trimming).
- `Error::RetryExhausted` gains `source: Box<Error>`. Struct patterns need
  `..`.

## Binary patching is opt-in

`StealthConfig::default().patch_binary` is `false`. Stock Chrome and Chrome for
Testing contain none of the markers the patcher removed, and its only match was
inside Chrome's code. Nothing needs to change unless you launch a ChromeDriver-
modified build; then set `patch_binary = true`. Patched copies are cached in
`~/.cache/eoka/patched-chrome` on Linux.

## Element handles and text lookup

- `Element` is pinned to Chrome's backend node id. Handles now survive calls
  that re-request the document, such as `click_shadow`. They still fail once
  the node is removed or the page navigates.
- `find_by_text` returns the innermost element containing the text, not the
  first container in document order. `find_all_by_text` no longer returns
  ancestors of a match.

## `try_*` and navigation

- `try_click` and the other `try_*` calls return `Ok(false)` when the target
  moved, became covered or went stale before any input was sent. They used to
  return `Err`.
- `goto` to a URL Chrome turns into a download, or a navigation another one
  replaces, fails with `navigation_aborted` right away. Use
  `Expectation::DownloadStarted` to wait for a download.
- `Browser::new_page` verifies navigation like `Page::goto` and closes the tab
  when it fails.

## Typing

`InputMode` has a new `Human` variant. Exhaustive matches need an arm for it.
`human_type` and `human_fill` now use it, so they are slower than `type_into`
and `fill`, and send trusted `keydown`/`keyup` events.

## Deprecated configuration

`StealthConfig::human_mouse`, `human_typing` and `debug` never had an effect
and are deprecated. Choose human input per call (`human_click`, `human_type`,
`InputMode::Human`); `debug_dir` still sets where `debug_screenshot` writes.
`aggressive_cdp_evasion` is a no-op, and `CDP_EVASION`, `TIMEZONE_EVASION`
and `WEBRTC_EVASION` are empty strings. `FINGERPRINT_EVASION` is now
`WEBGL_EVASION`, `CANVAS_EVASION` and `AUDIO_EVASION` joined together.

## Cookies

`SessionCookie` has a new `partition_key: Option<CookiePartitionKey>` field and
implements `Default`. Struct literals need `partition_key: None` or
`..Default::default()`. Session files written by 0.10 still load.
`cookies_for_url` returns a partitioned cookie only for a request from its own
top-level site.

`restore_state` now returns `Error::Navigation` when the saved URL redirects to
another origin and there is storage to write (cookies are already restored).
On live sessions it merges storage instead of clearing it.

## Request capture

`Page::capture_requests(RequestCaptureOptions::default())` returns a
page-scoped `RequestCapture`. Its request ids work with `get_response_body`.
`enable_request_capture` still only enables the Network domain.

## Low-level CDP access

- `Transport::subscribe()` returns `broadcast::Receiver<Arc<CdpMessage>>`.
  Match on `&*event` or `event.as_ref()`. The buffer holds
  `EVENT_CHANNEL_CAPACITY` (4096) events.
- `Transport::recv_event` and `try_recv_event` are removed; use `subscribe()`.
- Commands blocked by `filter_cdp` return `Error::Cdp` naming `filter_cdp`
  instead of `{}`. Launched browsers now honour `filter_cdp = false`.
- Commands on a detached session fail right away with
  `Error::Cdp { code: Some(-32001), .. }`.
- `eoka::cdp::types::Cookie` and `NetworkSetCookie` have a `partition_key`
  field.

## Launch flags

User `--disable-features`, `--enable-features`, `--disable-blink-features`,
`--enable-blink-features` and `--blink-settings` values in `extra_args` are
merged with eoka's own instead of replacing them. With a proxy configured,
Chrome runs with `--webrtc-ip-handling-policy=disable_non_proxied_udp`.
