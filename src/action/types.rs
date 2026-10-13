use std::{future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::Result;

/// A query evaluated afresh immediately before an action.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LocatorQuery {
    /// Resolve a pinned frontend node from an Element handle.
    Node {
        /// Chrome DOM node identifier for the original document.
        id: i32,
    },
    /// Match a CSS selector.
    Css {
        /// CSS selector evaluated when the action runs.
        selector: String,
    },
    /// Match element text.
    Text {
        /// Text to match or insert.
        text: String,
        #[serde(default)]
        /// Require an exact, trimmed text match.
        exact: bool,
    },
    /// Match an exact placeholder attribute.
    Placeholder {
        /// Text to match or insert.
        text: String,
    },
    /// Match an explicit ARIA role.
    Role {
        /// Explicit ARIA role to match.
        role: String,
    },
    /// Resolve a pinned node without retargeting a replacement.
    BackendNode {
        /// Pinned Chrome backend node identifier.
        id: i64,
    },
}

/// A serializable locator. Frame identifiers and backend nodes are pinned identities.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocatorSpec {
    /// Query used to resolve the target.
    pub query: LocatorQuery,
    #[serde(default)]
    /// Pinned frame identifier, or the main frame when absent.
    pub frame_id: Option<String>,
    #[serde(default)]
    /// Explicit zero-based match index; absence requires a unique match.
    pub index: Option<usize>,
}

impl LocatorSpec {
    /// Create a strict CSS locator specification.
    pub fn css(selector: impl Into<String>) -> Self {
        Self {
            query: LocatorQuery::Css {
                selector: selector.into(),
            },
            frame_id: None,
            index: None,
        }
    }

    /// Create a strict text locator specification.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            query: LocatorQuery::Text {
                text: text.into(),
                exact: false,
            },
            frame_id: None,
            index: None,
        }
    }
}

/// An observable completion condition. Values are not copied into receipts.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expectation {
    /// Observe a new native download originating from this page.
    DownloadStarted,
    /// Wait for a target to hold focus.
    Focused {
        /// Target that must receive focus.
        target: LocatorSpec,
    },
    /// Wait for the page URL.
    Url {
        /// Expected or requested value.
        value: String,
        #[serde(default)]
        /// Accept a substring instead of an exact match.
        contains: bool,
    },
    /// Wait for a rendered target.
    Visible {
        /// Locator resolved at execution time.
        target: LocatorSpec,
    },
    /// Wait for a target to disappear or stop rendering.
    Hidden {
        /// Locator resolved at execution time.
        target: LocatorSpec,
    },
    /// Match element text.
    Text {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        /// Expected or requested value.
        value: String,
        #[serde(default)]
        /// Accept a substring instead of an exact match.
        contains: bool,
    },
    /// Wait for a form value.
    Value {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        /// Expected or requested value.
        value: String,
    },
    /// Wait for a checked state.
    Checked {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        /// Expected checked state.
        checked: bool,
    },
    /// Wait for selected option values.
    Selected {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        /// Requested option values.
        values: Vec<String>,
    },
}

impl Expectation {
    /// Return the stable kind name without sensitive values.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::DownloadStarted => "download_started",
            Self::Focused { .. } => "focused",
            Self::Url { .. } => "url",
            Self::Visible { .. } => "visible",
            Self::Hidden { .. } => "hidden",
            Self::Text { .. } => "text",
            Self::Value { .. } => "value",
            Self::Checked { .. } => "checked",
            Self::Selected { .. } => "selected",
        }
    }
}

/// Native input or document action to perform once.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Navigate by a relative history offset; zero reloads the current document.
    History {
        /// Signed history offset.
        delta: i32,
    },
    /// Select one option by value, falling back to an exact visible label.
    SelectOption {
        /// Select control resolved at execution time.
        target: LocatorSpec,
        /// Preferred value or exact option label.
        value: String,
    },
    /// Navigate with temporary headers, restoring empty headers even on cancellation.
    NavigateWithHeaders {
        /// Destination URL.
        url: String,
        /// Temporary request headers; omitted from receipts.
        headers: std::collections::HashMap<String, String>,
    },
    /// Drag an actionable target by a viewport offset.
    Drag {
        /// Target at which to begin the drag.
        target: LocatorSpec,
        /// Horizontal offset in CSS pixels.
        dx: f64,
        /// Vertical offset in CSS pixels.
        dy: f64,
    },
    /// Insert text into the currently focused control.
    InsertText {
        /// Text to insert.
        text: String,
    },
    /// Select an option by its exact visible label.
    SelectText {
        /// Select control to resolve.
        target: LocatorSpec,
        /// Visible label of the option.
        text: String,
    },
    /// Click an actionable target once.
    Click {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        #[serde(default)]
        /// Use paced native input.
        human: bool,
        #[serde(default)]
        /// Dispatch a native double click.
        double: bool,
    },
    /// Replace a form value and verify it.
    Fill {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        /// Expected or requested value.
        value: String,
        #[serde(default)]
        /// Use paced native input.
        human: bool,
    },
    /// Insert text into a target and verify an explicit condition.
    Type {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        /// Text to match or insert.
        text: String,
        #[serde(default)]
        /// Use paced native input.
        human: bool,
    },
    /// Select options and verify their values.
    Select {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        /// Requested option values.
        values: Vec<String>,
    },
    /// Set file inputs and verify file names and sizes.
    Upload {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        /// Local file paths to upload.
        files: Vec<String>,
    },
    /// Send a key combination and verify an explicit condition.
    PressKey {
        /// Key or modifier combination, such as Ctrl+Enter.
        key: String,
    },
    /// Focus a target and press Enter.
    Submit {
        /// Locator resolved at execution time.
        target: LocatorSpec,
    },
    /// Move the pointer to a target and verify hover.
    Hover {
        /// Locator resolved at execution time.
        target: LocatorSpec,
        #[serde(default)]
        /// Use paced native input.
        human: bool,
    },
    /// Focus a target and verify focus.
    Focus {
        /// Locator resolved at execution time.
        target: LocatorSpec,
    },
    /// Scroll a target into the viewport.
    ScrollIntoView {
        /// Locator resolved at execution time.
        target: LocatorSpec,
    },
    /// Navigate and verify a document commit.
    Navigate {
        /// Destination URL.
        url: String,
        #[serde(default)]
        /// Optional navigation referrer.
        referrer: Option<String>,
    },
    /// Reload and verify a document commit.
    Reload,
    /// Navigate to the previous history entry.
    Back,
    /// Navigate to the next history entry.
    Forward,
}

impl Action {
    /// Return the stable kind name without sensitive values.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Drag { .. } => "drag",
            Self::InsertText { .. } => "insert_text",
            Self::SelectText { .. } => "select_text",
            Self::SelectOption { .. } => "select_option",
            Self::Click { double: true, .. } => "double_click",
            Self::Click { .. } => "click",
            Self::Fill { .. } => "fill",
            Self::Type { .. } => "type",
            Self::Select { .. } => "select",
            Self::Upload { .. } => "upload",
            Self::PressKey { .. } => "press_key",
            Self::Submit { .. } => "submit",
            Self::Hover { .. } => "hover",
            Self::Focus { .. } => "focus",
            Self::ScrollIntoView { .. } => "scroll_into_view",
            Self::Navigate { .. } | Self::NavigateWithHeaders { .. } => "navigate",
            Self::Reload => "reload",
            Self::History { .. } => "history",
            Self::Back => "back",
            Self::Forward => "forward",
        }
    }

    /// Return the action target, if any.
    pub fn target(&self) -> Option<&LocatorSpec> {
        match self {
            Self::Drag { target, .. }
            | Self::SelectText { target, .. }
            | Self::SelectOption { target, .. }
            | Self::Click { target, .. }
            | Self::Fill { target, .. }
            | Self::Type { target, .. }
            | Self::Select { target, .. }
            | Self::Upload { target, .. }
            | Self::Submit { target }
            | Self::Hover { target, .. }
            | Self::Focus { target }
            | Self::ScrollIntoView { target } => Some(target),
            _ => None,
        }
    }

    /// Whether this action requires caller-supplied completion evidence.
    pub fn requires_expectation(&self) -> bool {
        matches!(
            self,
            Self::Click { .. }
                | Self::Type { .. }
                | Self::PressKey { .. }
                | Self::Submit { .. }
                | Self::Drag { .. }
                | Self::InsertText { .. }
        )
    }
}

/// Persist dispatch intent before allowing any potentially mutating command.
/// Implementations must return an error when intent cannot be made durable.
pub trait DispatchObserver: Send + Sync {
    /// Persist intent before a command can execute; preparation is tracked separately.
    fn before_dispatch(
        &self,
        preparation: bool,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

/// One deadline includes queueing, preparation, dispatch and verification.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Serialize, Deserialize)]
pub struct ActionOptions {
    /// Wait for target actionability; false makes try-actions skip immediately.
    #[serde(default = "wait_for_target")]
    pub wait_for_target: bool,
    #[serde(default = "default_timeout")]
    /// Total action deadline in milliseconds, including queueing.
    pub timeout_ms: u64,
    #[serde(default)]
    /// Completion condition; required for actions without an intrinsic result.
    pub expect: Option<Expectation>,
    #[serde(skip)]
    /// Optional durable intent hook, invoked before mutation.
    pub observer: Option<Arc<dyn DispatchObserver>>,
}

fn wait_for_target() -> bool {
    true
}

fn default_timeout() -> u64 {
    30_000
}

impl Default for ActionOptions {
    fn default() -> Self {
        Self {
            wait_for_target: true,
            timeout_ms: default_timeout(),
            expect: None,
            observer: None,
        }
    }
}

impl ActionOptions {
    /// Create default options with an explicit completion condition.
    pub fn expecting(expect: Expectation) -> Self {
        Self {
            expect: Some(expect),
            ..Self::default()
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Whether completion is proven, uncertain, or undispatched.
pub enum ActionOutcome {
    /// No primary command was sent; preparation may have occurred.
    NotDispatched,
    /// A primary command may have run without verified completion.
    OutcomeUnknown,
    /// The requested completion condition was observed.
    VerifiedComplete,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Transport evidence for the primary action commands.
pub enum DispatchState {
    /// No primary command reached transport dispatch.
    NotSent,
    /// Dispatch began but at least one acknowledgment is missing.
    MayHaveBeenSent,
    /// Every sent primary command was acknowledged by Chrome.
    Acknowledged,
}

/// Evidence about an action, deliberately excluding input values and page text.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionReceipt {
    /// Unix time at admission, in milliseconds.
    pub started_at_ms: u64,
    /// Unix time when this receipt was produced, in milliseconds.
    pub finished_at_ms: u64,
    /// Native GUID observed for a download expectation.
    pub download_guid: Option<String>,
    /// Action kind, without submitted values.
    pub action: String,
    /// Chrome target identifier.
    pub target_id: String,
    /// Conservative classification of the result.
    pub outcome: ActionOutcome,
    /// Evidence about primary command dispatch.
    pub dispatch: DispatchState,
    /// Whether focus, scrolling, or other preparatory input may have run.
    pub preparation_may_have_executed: bool,
    /// Kind of completion evidence observed, without sensitive values.
    pub verification: Option<String>,
    /// Elapsed wall-clock time in milliseconds.
    pub elapsed_ms: u64,
    /// Whether the intrinsic result required no mutation.
    pub already_satisfied: bool,
    /// Whether input release is continuing after return.
    pub cleanup_pending: bool,
}

impl std::fmt::Debug for ActionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionOptions")
            .field("timeout_ms", &self.timeout_ms)
            .field("expect", &self.expect.as_ref().map(Expectation::kind))
            .finish_non_exhaustive()
    }
}
