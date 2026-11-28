mod action;
mod coordinated;
mod drag;
mod held;
pub(crate) use action::ActionInput;
pub(crate) use coordinated::{
    coordinated_key_char, coordinated_key_down, coordinated_key_up, coordinated_mouse_down,
    coordinated_mouse_move, coordinated_mouse_up, coordinated_mouse_wheel, held_mouse_down,
    held_mouse_move, held_mouse_up,
};
use coordinated::{dispatch_coordinated_key_event, dispatch_coordinated_mouse_event, key_event};
pub(crate) use drag::DragInput;
pub(crate) use held::HeldInputState;
use held::{held_key_from_combo, input_state_error};

use super::{sleep_ms, MouseButton, Page, INTERACTION_DELAY_MS};
use crate::action::ActionCall;
use crate::cdp::{MouseButton as CdpMouseButton, MouseEventType, Session};
use crate::error::Result;
use crate::stealth::Human;

impl MouseButton {
    fn cdp(self) -> CdpMouseButton {
        match self {
            Self::Left => CdpMouseButton::Left,
            Self::Middle => CdpMouseButton::Middle,
            Self::Right => CdpMouseButton::Right,
            Self::Back => CdpMouseButton::Back,
            Self::Forward => CdpMouseButton::Forward,
        }
    }

    fn bit(self) -> i32 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
            Self::Back => 8,
            Self::Forward => 16,
        }
    }
}
impl Page {
    /// Click at coordinates with the primary mouse button.
    pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
        self.mouse_down(x, y, MouseButton::Left).await?;
        sleep_ms(INTERACTION_DELAY_MS).await;
        self.mouse_up(x, y, MouseButton::Left).await
    }

    /// Press and hold a mouse button at viewport coordinates.
    ///
    /// The press remains held until [`Page::mouse_up`] or
    /// [`Page::release_all_inputs`]. If this future is cancelled while CDP is
    /// in flight, its reservation is retained for `release_all_inputs`.
    /// Calling this twice for the same button returns [`Error::InputState`](crate::Error::InputState).
    pub async fn mouse_down(&self, x: f64, y: f64, button: MouseButton) -> Result<()> {
        coordinated_mouse_down(&self.session, &self.held_input, x, y, button).await
    }

    /// Move the mouse to viewport coordinates, preserving any held buttons.
    pub async fn mouse_move(&self, x: f64, y: f64) -> Result<()> {
        coordinated_mouse_move(&self.session, &self.held_input, x, y).await
    }

    /// Release a held mouse button at viewport coordinates.
    ///
    /// Local held state is cleared even if CDP rejects the release, so it
    /// cannot remain permanently tracked after a transport error.
    pub async fn mouse_up(&self, x: f64, y: f64, button: MouseButton) -> Result<()> {
        coordinated_mouse_up(&self.session, &self.held_input, x, y, button).await
    }

    /// Release every held key and mouse button.
    ///
    /// This is the async cleanup path for interrupted drag/key operations.
    /// Call it before closing a tab or browser; Rust `Drop` cannot reliably
    /// send asynchronous CDP releases. If this future is cancelled, unhandled
    /// inputs remain recorded so a later call can retry their releases.
    pub async fn release_all_inputs(&self) -> Result<()> {
        let mut state = self.held_input.lock().await;
        let mut first_error = None;

        // Inputs are removed only after their release command completes. A
        // cancelled cleanup therefore retains the remaining inputs for retry.
        let key_ids = state.keys.keys().cloned().collect::<Vec<_>>();
        for id in key_ids {
            let (key, modifiers) = match state.key_up_event(&id) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if let Err(error) = dispatch_coordinated_key_event(
                &self.session,
                &key,
                crate::cdp::KeyEventType::KeyUp,
                modifiers,
            )
            .await
            {
                first_error.get_or_insert(error);
                continue;
            }
            state.finish_key_up(&id);
        }

        let mouse_buttons = state
            .mouse_buttons
            .iter()
            .map(|(button, position)| (*button, *position))
            .collect::<Vec<_>>();
        for (button, (x, y)) in mouse_buttons {
            let buttons = match state.mouse_up_mask(button) {
                Ok(buttons) => buttons,
                Err(_) => continue,
            };
            if let Err(error) = dispatch_coordinated_mouse_event(
                &self.session,
                MouseEventType::MouseReleased,
                x,
                y,
                Some(button),
                Some(1),
                buttons,
            )
            .await
            {
                first_error.get_or_insert(error);
                continue;
            }
            state.finish_mouse_up(button);
            state.pointer = Some((x, y));
        }

        match first_error {
            Some(error) => Err(error),
            None => {
                self.session
                    .action_state()
                    .cleanup_failed
                    .store(false, std::sync::atomic::Ordering::Release);
                Ok(())
            }
        }
    }

    /// Type text into the focused element. Awaiting proves dispatch only; see [`ActionCall`].
    pub fn type_text(&self, text: &str) -> ActionCall {
        ActionCall::new(self, crate::Action::InsertText { text: text.into() })
    }

    /// Get a Human helper for human-like interactions.
    ///
    /// Its pointer actions share this Page's held-input coordinator.
    pub fn human(&self) -> Human<'_> {
        Human::new(&self.session)
    }

    /// Press key with optional modifiers (e.g., "Enter", "Ctrl+A", "Cmd+Shift+S").
    ///
    /// Printable keys retain their case and carry text on key-down, so Chrome
    /// generates native editing events unless the page cancels the default action.
    /// Shift uses US-keyboard ASCII mappings; other single Unicode characters are
    /// sent literally. Control, Alt and Meta suppress text. For complete strings
    /// or composed text, use [`Page::type_text`] rather than a key combination.
    /// Awaiting proves dispatch only; see [`ActionCall`].
    pub fn press_key(&self, key: &str) -> ActionCall {
        ActionCall::new(self, crate::Action::PressKey { key: key.into() })
    }

    /// Dispatch a key-down/up sequence without verifying an application outcome.
    /// This primitive preserves manually held modifiers. Use `act` for consequential actions.
    pub async fn dispatch_key(&self, key: &str) -> Result<()> {
        self.key_down(key).await?;
        self.key_up(key).await
    }

    /// Press and hold a key, optionally including modifiers in `key`.
    ///
    /// For example, `key_down("Ctrl+A")` dispatches `A` with the Control
    /// modifier. To hold a modifier across calls, use `key_down("Ctrl")`,
    /// then call `key_down("A")`. Printable key-downs can insert text; key-up and
    /// cleanup never insert text. A literal `"A"` means uppercase, while `"a"`
    /// means lowercase unless Shift is held. Duplicate key-down calls return
    /// [`Error::InputState`](crate::Error::InputState).
    pub async fn key_down(&self, key: &str) -> Result<()> {
        coordinated_key_down(&self.session, &self.held_input, key).await
    }

    /// Release a key previously held with [`Page::key_down`].
    ///
    /// Local held state is cleared even if CDP rejects the release.
    pub async fn key_up(&self, key: &str) -> Result<()> {
        coordinated_key_up(&self.session, &self.held_input, key).await
    }

    /// Platform-aware select all (Cmd+A on Mac, Ctrl+A elsewhere)
    pub fn select_all(&self) -> ActionCall {
        self.press_key(if cfg!(target_os = "macos") {
            "Cmd+A"
        } else {
            "Ctrl+A"
        })
    }

    /// Platform-aware copy (Cmd+C on Mac, Ctrl+C elsewhere)
    pub fn copy(&self) -> ActionCall {
        self.press_key(if cfg!(target_os = "macos") {
            "Cmd+C"
        } else {
            "Ctrl+C"
        })
    }

    /// Platform-aware paste (Cmd+V on Mac, Ctrl+V elsewhere)
    pub fn paste(&self) -> ActionCall {
        self.press_key(if cfg!(target_os = "macos") {
            "Cmd+V"
        } else {
            "Ctrl+V"
        })
    }
}
