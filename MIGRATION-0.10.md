# Migrating to eoka 0.10

## Shorthand input methods work without an expectation

Before 0.10, `page.click("#save").await` failed with `expectation_required` unless
the page handle was configured with an expectation. It now dispatches the click and
tells you what is actually known:

```rust,ignore
// Dispatched to an actionable target. Whether the page reacted is unknown.
let receipt = page.click("#save").await?;
assert_eq!(receipt.outcome, ActionOutcome::OutcomeUnknown);

// Proven for this call: VerifiedComplete, or Err carrying a receipt.
page.click("#save")
    .expecting(Expectation::Visible { target: LocatorSpec::css("#saved") })
    .await?;
```

**`Ok` from a plain call proves dispatch only.** Use `.expecting(..)`, or `Page::act`
to require evidence. `Page::act` and `Locator` are unchanged and still reject these
actions without an expectation. Missing, covered and disabled targets are still
errors, and actions are still never replayed after dispatch.

## Changed signatures

These are now plain functions returning `ActionCall`, which runs when awaited:

- `Page`: `click`, `click_by_text`, `human_click`, `human_click_by_text`, `type_into`,
  `human_type`, `type_text`, `press_key`, `select_all`, `copy`, `paste`, `human_drag`
- `Element`: `click`, `human_click`, `human_drag_by`, `type_text`

`try_click`, `try_click_by_text`, `try_human_click` and `try_human_click_by_text`
return `TryActionCall`, which awaits to `Result<bool>`. They now return `Ok(false)`
for an unavailable target on any handle, meaning no input was sent, though an
off-screen target may have been scrolled to. `Ok(true)` means dispatched, and proves
the result only with `.expecting(..)`.

`type_text` now fails with `Error::InputState` unless an editable element has focus,
because the text would otherwise be lost. `press_key` still succeeds with no focus,
since keys can drive page shortcuts.

Most `.await?` call sites compile unchanged. Two cases need an edit:

```rust,ignore
// The await now yields a receipt, so a Result<()> tail expression needs mapping.
async fn save(page: &Page) -> eoka::Result<()> {
    page.click("#save").await.map(|_| ())
}

// Calls are IntoFuture, not Future. APIs that require a Future need conversion,
// including tokio::spawn and with_retry closures.
use std::future::IntoFuture;
tokio::spawn(page.click("#save").into_future());
```

A call does nothing until awaited, and dropping one unawaited sends no input.

## `with_action_options` is replaced

An expectation describes one operation, so a page handle can no longer hold one.
`Page::with_action_options` is removed and is a compile error, so no expectation is
silently ignored. `ActionOptions` itself is unchanged for `Page::act`, `explore` and
`Locator`.

```rust,ignore
// Before
page.with_action_options(ActionOptions::expecting(saved.clone()))
    .click("#save")
    .await?;

// After
page.click("#save").expecting(saved).await?;
```

Elements used to inherit the expectation of the handle that found them. Now set it
on the call: `element.click().expecting(saved).await?`.

For other settings, use `Page::with_action_defaults`:

```rust,ignore
let slow = page.with_action_defaults(ActionDefaults {
    timeout_ms: 120_000,
    ..Default::default()
});
```

Intrinsically verified methods such as `fill`, `select` and `goto` can no longer pick
up an additional expectation from the handle. Use `Page::act` for those cases.

## Cookie export is scoped to a URL

`BrowserSession::cookie_header()` is removed. It sent every cookie to whichever host
you called, and percent-encoded the values.

```rust,ignore
// Before: credentials for every site, with altered values
let header = session.cookie_header();

// After: only cookies matching this URL, byte for byte
let header = session.cookie_header_for("https://api.example.com/v1/items");
```

The result is empty when no cookie applies, or when the URL can't be parsed safely.
Paths aren't normalized, so resolve `..` segments first. Domains aren't checked against
the public suffix list, so only load session files you trust.
`Secure` cookies require https or wss. If you relied on the encoding, encode values
yourself; a server that set `tok%3Dabc` expects to receive exactly that.

`Page::capture_state` now works like Playwright's `storageState`: it saves cookies for
every site in the page's browser context, not just the current URL. That includes a
browser you attached to, so protect saved states accordingly. Storage remains limited
to the current origin. `Page::cookies` still
returns the current URL's cookies, and `Page::all_cookies` returns every site's.
`Page::restore_state` still replaces the whole jar, and restoring a state saved by an
earlier version leaves only that one site's cookies. The exception is a live session
(`StealthConfig::live`, `Browser::connect`, or a reused running profile): it no longer
deletes anything, and only adds or overwrites the saved cookies.

`BrowserSession::save` now creates owner-only files on Unix and replaces the target
atomically, so a symlink at that path is replaced rather than followed.
