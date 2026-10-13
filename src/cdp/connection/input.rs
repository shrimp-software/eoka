//! Input commands on a CDP session.

use super::Session;
use crate::cdp::types::*;
use crate::Result;

impl Session {
    /// Dispatch a raw mouse event with full control over all fields
    pub(crate) async fn dispatch_mouse_event_full(
        &self,
        event: InputDispatchMouseEvent,
    ) -> Result<()> {
        self.send_void("Input.dispatchMouseEvent", &event).await
    }

    /// Dispatch a key event with full modifier support
    pub(crate) async fn dispatch_key_event_full(
        &self,
        event: InputDispatchKeyEventFull,
    ) -> Result<()> {
        self.send_void("Input.dispatchKeyEvent", &event).await
    }
}
