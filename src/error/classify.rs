//! One place that interprets Chrome's CDP error responses.
//!
//! Chrome reports these conditions only through message text, so every
//! string match against CDP errors lives here.

use super::Error;

/// What a CDP error says about the object, context or session it targeted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CdpFailure {
    /// The session or target is gone (tab closed, detached).
    SessionGone,
    /// The JavaScript context or a remote object in it was destroyed, usually
    /// by navigation. Retrying in the new document can succeed.
    ContextLost,
    /// A node id no longer refers to a node in the document.
    NodeGone,
    /// The node exists but has no layout box.
    NotRendered,
    /// The id refers to a node that is not an element.
    NotElement,
}

/// Classifies a CDP error. `None` means it is not one of the known lifecycle
/// conditions and should be treated as a real failure.
pub(crate) fn cdp_failure(error: &Error) -> Option<CdpFailure> {
    let Error::Cdp { message, .. } = error else {
        return None;
    };
    let message = message.as_str();
    let any = |needles: &[&str]| needles.iter().any(|needle| message.contains(needle));
    if any(&[
        "Session with given id not found",
        "No target with given id found",
        "Not attached to an active page",
        "Inspected target navigated or closed",
        "Target closed",
    ]) {
        Some(CdpFailure::SessionGone)
    } else if any(&[
        "Cannot find context",
        "Execution context was destroyed",
        "Could not find object",
        "Cannot find object",
        "Node with given id does not belong to the document",
    ]) {
        Some(CdpFailure::ContextLost)
    } else if any(&["Could not find node", "No node with given id"]) {
        Some(CdpFailure::NodeGone)
    } else if message.contains("box model") {
        Some(CdpFailure::NotRendered)
    } else if message.contains("Node is not an element") {
        Some(CdpFailure::NotElement)
    } else {
        None
    }
}

pub(crate) fn is(error: &Error, failure: CdpFailure) -> bool {
    cdp_failure(error) == Some(failure)
}

fn cdp_message_contains(error: &Error, needles: &[&str]) -> bool {
    matches!(error, Error::Cdp { message, .. } if needles.iter().any(|needle| message.contains(needle)))
}

/// Whether a frame snapshot raced a frame target or session going away
/// (frame discovery retries these). Narrower than
/// [`CdpFailure::SessionGone`]: a closed or navigated inspected target is not
/// retried, and a vanished frame id is.
pub(crate) fn frame_discovery_raced(error: &Error) -> bool {
    cdp_message_contains(
        error,
        &[
            "No target with given id found",
            "Session with given id not found",
            "Not attached to an active page",
            "No frame with given id found",
        ],
    )
}

/// Whether an evaluation raced a navigation of the page (waits treat these as
/// "not yet"). Narrower than [`CdpFailure::ContextLost`]: lost remote objects
/// are not included.
pub(crate) fn is_navigation_race(error: &Error) -> bool {
    cdp_message_contains(
        error,
        &[
            "Cannot find context with specified id",
            "Execution context was destroyed",
            "Inspected target navigated or closed",
        ],
    )
}

/// Whether a node id refers to a document that was replaced. Narrower than
/// [`CdpFailure::ContextLost`]: lost remote objects are not included.
pub(crate) fn node_left_document(error: &Error) -> bool {
    cdp_message_contains(
        error,
        &["Node with given id does not belong to the document"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_chrome_messages() {
        let cases = [
            ("Session with given id not found.", CdpFailure::SessionGone),
            ("No target with given id found", CdpFailure::SessionGone),
            ("Execution context was destroyed.", CdpFailure::ContextLost),
            (
                "Cannot find context with specified id",
                CdpFailure::ContextLost,
            ),
            ("Could not find node with given id", CdpFailure::NodeGone),
            ("No node with given id found", CdpFailure::NodeGone),
            ("Could not compute box model.", CdpFailure::NotRendered),
            ("Node is not an element", CdpFailure::NotElement),
        ];
        for (message, expected) in cases {
            assert_eq!(
                cdp_failure(&Error::cdp("X.y", -32000, message)),
                Some(expected),
                "{message}"
            );
        }
        assert_eq!(
            cdp_failure(&Error::cdp("X.y", -32000, "Invalid selector")),
            None
        );
        assert_eq!(
            cdp_failure(&Error::Timeout("Execution context was destroyed".into())),
            None
        );
    }

    #[test]
    fn only_known_discovery_races_are_retried() {
        for message in [
            "No target with given id found",
            "Session with given id not found",
            "Not attached to an active page",
            "No frame with given id found",
        ] {
            assert!(frame_discovery_raced(&Error::cdp_msg(message)), "{message}");
        }
        for message in [
            "Invalid parameters",
            "Target closed",
            "Inspected target navigated or closed",
        ] {
            assert!(
                !frame_discovery_raced(&Error::cdp_msg(message)),
                "{message}"
            );
        }
        assert!(!frame_discovery_raced(&Error::Decode(
            "No target with given id found".into()
        )));
    }

    #[test]
    fn node_left_document_excludes_lost_objects() {
        assert!(node_left_document(&Error::cdp(
            "DOM.resolveNode",
            -32000,
            "Node with given id does not belong to the document"
        )));
        assert!(!node_left_document(&Error::cdp_msg("Cannot find object")));
        assert!(!node_left_document(&Error::cdp_msg(
            "Could not find object"
        )));
        assert!(!node_left_document(&Error::Decode(
            "Node with given id does not belong to the document".into()
        )));
    }
}

/// Why Chrome refused `Page.startScreenRecording`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingStartFailure {
    /// This Chrome build has no native recorder (method missing or disabled).
    Unsupported,
    /// Chrome already records this target.
    AlreadyActive,
    /// The target is inactive, not top-level or not attached.
    Precondition,
}

/// Classifies a failed `Page.startScreenRecording`. Kept apart from
/// [`cdp_failure`] because its needles ("inactive", "not attached") overlap
/// the lifecycle conditions there.
pub(crate) fn recording_start_failure(error: &Error) -> Option<RecordingStartFailure> {
    let Error::Cdp { code, message, .. } = error else {
        return None;
    };
    let text = message.to_ascii_lowercase();
    if *code == Some(-32601) || text.contains("not supported") || text.contains("wasn't found") {
        return Some(RecordingStartFailure::Unsupported);
    }
    if text.contains("already active") {
        return Some(RecordingStartFailure::AlreadyActive);
    }
    if text.contains("inactive")
        || text.contains("top-level")
        || text.contains("top target")
        || text.contains("not attached")
    {
        return Some(RecordingStartFailure::Precondition);
    }
    None
}

#[cfg(test)]
mod recording_tests {
    use super::*;

    #[test]
    fn classifies_recording_start_failures() {
        let cases = [
            (
                -32601,
                "'Page.startScreenRecording' wasn't found",
                Some(RecordingStartFailure::Unsupported),
            ),
            (
                -32000,
                "Screen recording is not supported",
                Some(RecordingStartFailure::Unsupported),
            ),
            (
                -32000,
                "Recording is already active",
                Some(RecordingStartFailure::AlreadyActive),
            ),
            (
                -32000,
                "Target is inactive",
                Some(RecordingStartFailure::Precondition),
            ),
            (
                -32000,
                "Only top-level targets can record",
                Some(RecordingStartFailure::Precondition),
            ),
            (
                -32000,
                "Not attached to an active page",
                Some(RecordingStartFailure::Precondition),
            ),
            (-32000, "Something else", None),
        ];
        for (code, message, expected) in cases {
            assert_eq!(
                recording_start_failure(&Error::cdp("Page.startScreenRecording", code, message)),
                expected,
                "{message}"
            );
        }
        assert_eq!(
            recording_start_failure(&Error::Timeout("not supported".into())),
            None
        );
    }
}
