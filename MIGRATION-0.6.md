# Eoka 0.6: dependable actions

This is a breaking release. High-level actions resolve a strict, lazy locator,
prepare the target, dispatch once, and verify a result under one deadline. The
default is 30 seconds, including time waiting for another action on the target.
DOM waits also bound slow browser commands, not just polling intervals.

```rust,no_run
use eoka::{ActionOptions, ActionOutcome, Browser, Expectation, LocatorSpec};

# async fn example() -> eoka::Result<()> {
let browser = Browser::launch().await?;
let page = browser.new_page("https://example.com/form").await?;
page.locator("#name").fill("Ada", ActionOptions::default()).await?;
let receipt = page.locator("#save").click(ActionOptions {
    timeout_ms: 5_000,
    ..ActionOptions::expecting(Expectation::Text {
        target: LocatorSpec::css("#status"),
        value: "Saved".into(),
        contains: false,
    })
}).await?;
assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
# browser.close().await?;
# Ok(()) }
```

## Existing methods

`Page::click`, text clicks, human clicks, `type_into`, `type_text`, `human_type`,
`press_key`, and drag now require an expectation. Configure a page handle with
`page.with_action_options(ActionOptions::expecting(...))`, or use `Page::act`
and the `Action` enum to receive a success receipt. Element methods inherit the
options of the page that found them. An `Element` remains pinned to its original
node and fails when stale; a locator can find its replacement before dispatch.

Fill verifies the value, select verifies selected values, upload verifies the
FileList names and sizes, focus verifies focus, hover observes trusted mouse
input on the target, and scroll verifies viewport intersection. Navigation,
including temporary-header navigation, verifies a navigation commit. These
actions have intrinsic expectations; an additional explicit expectation can
check application readiness. A fill/select already holding the requested value
returns a verified receipt with `already_satisfied: true` and no primary input.

Click, typing, key, submit and drag require a caller-supplied condition because
Chrome acknowledging input does not establish application success. An explicit
condition already true before preparation is rejected. Verification establishes
that the chosen observable condition was reached; it does not prove a business
transaction committed or establish causality against independent page changes.

Multiple matches fail with `ambiguous_target`. Use `.first()` or `.nth(index)`
explicitly, or set `LocatorSpec.index` in serialized actions. `in_frame(frame_id)`
pins frame identity. CSS/text/placeholder queries resolve during execution;
role queries currently match explicit ARIA roles. Input in frames also checks
the iframe owners for interception. Unsupported frame transforms fail closed.

Try-click skips a missing or non-actionable target before dispatch and propagates
ambiguous targets, missing expectations and uncertain results. `with_retry`
never retries an `Error::Action`. Replace retry-wrapped clicks with one locator
action and its deadline.

## Outcome evidence and cancellation

An `Error::Action { code, receipt, source }` retains the same receipt format as
a successful action. Outcomes are `not_dispatched`, `outcome_unknown`, and
`verified_complete`. Dispatch evidence separately distinguishes `not_sent`,
`may_have_been_sent`, and `acknowledged`. Acknowledgment alone is insufficient
for verified completion. Preparation, such as scrolling, focus and pointer
movement, is recorded separately and may occur even for `not_dispatched`.

Timeout or cancellation after primary dispatch never replays the action.
Cancellation schedules bounded key/button release while retaining the target's
action gate. Failed cleanup blocks later high-level actions until
`release_all_inputs()` succeeds. High-level actions reject preexisting held
input. Low-level `mouse_down/up`, `key_down/up`, `dispatch_key`, `click_at`,
`Human`, JavaScript execution and raw CDP remain explicit primitives; they do
not provide verified action receipts or durable operation semantics.

The library does not persist history. `DispatchObserver` lets an owner persist
intent before each mutation. Eoka-tools supplies that owner, stable operation
IDs and durable records. Receipts omit selectors, input values, page text and
expected values; they retain action kind, target ID, timestamps, dispatch state,
verification kind and an observed download GUID where applicable.

`DownloadStarted` requires native download events to be enabled beforehand.
It proves a transfer began, not that a file is complete. Eoka-tools manages
exclusive browser download ownership, popup correlation, transfer lifetime,
file verification and restart recovery. `navigate_for_download` remains a
low-level attachment-navigation primitive.

## Validation and paired release

The browser regressions in `tests/actions.rs` exercise ambiguity, dynamic
replacement, visibility, overlays, movement, queue deadlines, uncertainty,
cancellation cleanup and stale handles. Run the full pre-push gates in
`CLAUDE.md`, including every ignored browser test and Rust 1.85 checks.

Publish core 0.6 before the matching eoka-tools crates. During joint development,
use a command-line Cargo patch in eoka-tools:

```sh
cargo test --workspace --all-features --config 'patch.crates-io.eoka.path="../eoka"'
```

No release or installed-daemon replacement is performed by building this branch.
