mod drag;
pub(crate) use drag::DragInput;

use std::collections::HashMap;
use std::sync::Arc;

use super::{sleep_ms, MouseButton, Page, INTERACTION_DELAY_MS};
use crate::cdp::{MouseButton as CdpMouseButton, MouseEventType, Session};
use crate::error::{Error, Result};
use crate::keyboard::{key_for_modifiers, key_text, key_to_codes, parse_key_combo};
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HeldKeyId {
    modifiers: i32,
    key: String,
}

#[derive(Debug, Clone)]
struct HeldKey {
    combo_modifiers: i32,
    key: String,
    code: String,
    virtual_key_code: Option<i32>,
    modifier_bit: i32,
}

#[derive(Default)]
pub(crate) struct HeldInputState {
    mouse_buttons: HashMap<MouseButton, (f64, f64)>,
    keys: HashMap<HeldKeyId, HeldKey>,
}

fn input_state_error(message: impl Into<String>) -> Error {
    Error::InputState(message.into())
}

fn modifier_bit_for_key(key: &str) -> i32 {
    use crate::cdp::modifiers;

    match key.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => modifiers::CTRL,
        "alt" | "option" => modifiers::ALT,
        "shift" => modifiers::SHIFT,
        "cmd" | "meta" | "command" => modifiers::META,
        _ => 0,
    }
}

fn held_key_from_combo(combo: &str) -> (HeldKeyId, HeldKey) {
    let (modifiers, key_name) = parse_key_combo(combo);
    let (key, code, virtual_key_code) = key_to_codes(key_name);
    let id = HeldKeyId {
        modifiers,
        key: key_name.to_ascii_lowercase(),
    };
    let held_key = HeldKey {
        combo_modifiers: modifiers,
        key: key.to_string(),
        code: code.to_string(),
        virtual_key_code,
        modifier_bit: modifier_bit_for_key(key_name),
    };
    (id, held_key)
}

impl HeldInputState {
    fn mouse_button_mask(&self) -> i32 {
        self.mouse_buttons
            .keys()
            .fold(0, |mask, button| mask | button.bit())
    }

    fn active_key_modifiers(&self) -> i32 {
        self.keys
            .values()
            .fold(0, |modifiers, key| modifiers | key.modifier_bit)
    }

    fn reserve_mouse_down(
        &mut self,
        button: MouseButton,
        position: (f64, f64),
    ) -> std::result::Result<i32, &'static str> {
        if self.mouse_buttons.contains_key(&button) {
            return Err("mouse button is already held");
        }
        // Retain this reservation before dispatch. If the future is cancelled
        // while CDP is in flight, release_all_inputs can still send its up.
        self.mouse_buttons.insert(button, position);
        Ok(self.mouse_button_mask())
    }

    fn cancel_mouse_down(&mut self, button: MouseButton) {
        self.mouse_buttons.remove(&button);
    }

    fn mouse_up_mask(&self, button: MouseButton) -> std::result::Result<i32, &'static str> {
        if !self.mouse_buttons.contains_key(&button) {
            return Err("mouse button is not held");
        }
        Ok(self.mouse_button_mask() & !button.bit())
    }

    fn finish_mouse_up(&mut self, button: MouseButton) {
        self.mouse_buttons.remove(&button);
    }

    fn reserve_key_down(
        &mut self,
        id: HeldKeyId,
        key: HeldKey,
    ) -> std::result::Result<i32, &'static str> {
        if self.keys.contains_key(&id) {
            return Err("key is already held");
        }
        // As with mouse reservations, retain the key across cancellation so
        // the explicit async cleanup path can release it.
        self.keys.insert(id, key);
        Ok(self.active_key_modifiers())
    }

    fn cancel_key_down(&mut self, id: &HeldKeyId) {
        self.keys.remove(id);
    }

    fn key_up_event(&self, id: &HeldKeyId) -> std::result::Result<(HeldKey, i32), &'static str> {
        let held_key = self.keys.get(id).cloned().ok_or("key is not held")?;
        // Read modifiers from the current held state rather than preserving
        // the key-down snapshot. The key being released contributes to its
        // own key-up event; subsequent releases observe its removal.
        let modifiers = self.active_key_modifiers() | held_key.combo_modifiers;
        Ok((held_key, modifiers))
    }

    fn finish_key_up(&mut self, id: &HeldKeyId) {
        self.keys.remove(id);
    }
}

pub(crate) async fn coordinated_mouse_move(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    x: f64,
    y: f64,
) -> Result<()> {
    let mut state = held_input.lock().await;
    let buttons = state.mouse_button_mask();
    dispatch_coordinated_mouse_event(
        session,
        MouseEventType::MouseMoved,
        x,
        y,
        None,
        None,
        buttons,
    )
    .await?;
    for position in state.mouse_buttons.values_mut() {
        *position = (x, y);
    }
    Ok(())
}

pub(crate) async fn coordinated_mouse_down(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    x: f64,
    y: f64,
    button: MouseButton,
) -> Result<()> {
    let mut state = held_input.lock().await;
    let buttons = state
        .reserve_mouse_down(button, (x, y))
        .map_err(input_state_error)?;
    let result = dispatch_coordinated_mouse_event(
        session,
        MouseEventType::MousePressed,
        x,
        y,
        Some(button),
        Some(1),
        buttons,
    )
    .await;
    if result.is_err() {
        state.cancel_mouse_down(button);
    }
    result
}

pub(crate) async fn coordinated_mouse_up(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    x: f64,
    y: f64,
    button: MouseButton,
) -> Result<()> {
    let mut state = held_input.lock().await;
    let buttons = state.mouse_up_mask(button).map_err(input_state_error)?;
    let result = dispatch_coordinated_mouse_event(
        session,
        MouseEventType::MouseReleased,
        x,
        y,
        Some(button),
        Some(1),
        buttons,
    )
    .await;
    state.finish_mouse_up(button);
    result
}

pub(crate) async fn coordinated_mouse_wheel(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    x: f64,
    y: f64,
    delta_x: f64,
    delta_y: f64,
) -> Result<()> {
    let state = held_input.lock().await;
    let buttons = state.mouse_button_mask();
    session
        .dispatch_mouse_event_full(crate::cdp::InputDispatchMouseEvent {
            r#type: MouseEventType::MouseWheel,
            x,
            y,
            button: None,
            click_count: None,
            buttons: (buttons != 0).then_some(buttons),
            force: None,
            delta_x: Some(delta_x),
            delta_y: Some(delta_y),
        })
        .await
}

pub(crate) async fn coordinated_key_down(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    key: &str,
) -> Result<()> {
    let (id, held_key) = held_key_from_combo(key);
    let mut state = held_input.lock().await;
    let modifiers = state
        .reserve_key_down(id.clone(), held_key.clone())
        .map_err(input_state_error)?
        | held_key.combo_modifiers;
    let result = dispatch_coordinated_key_event(
        session,
        &held_key,
        crate::cdp::KeyEventType::KeyDown,
        modifiers,
    )
    .await;
    if result.is_err() {
        state.cancel_key_down(&id);
    }
    result
}

pub(crate) async fn coordinated_key_up(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    key: &str,
) -> Result<()> {
    let (id, _) = held_key_from_combo(key);
    let mut state = held_input.lock().await;
    let (held_key, modifiers) = state.key_up_event(&id).map_err(input_state_error)?;
    let result = dispatch_coordinated_key_event(
        session,
        &held_key,
        crate::cdp::KeyEventType::KeyUp,
        modifiers,
    )
    .await;
    state.finish_key_up(&id);
    result
}

pub(crate) async fn coordinated_key_char(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    text: &str,
) -> Result<()> {
    let state = held_input.lock().await;
    let modifiers = state.active_key_modifiers();
    session
        .dispatch_key_event_full(crate::cdp::InputDispatchKeyEventFull {
            r#type: crate::cdp::KeyEventType::Char,
            modifiers: (modifiers != 0).then_some(modifiers),
            text: Some(text.to_string()),
            unmodified_text: Some(text.to_string()),
            ..Default::default()
        })
        .await
}

async fn dispatch_coordinated_mouse_event(
    session: &Session,
    event_type: MouseEventType,
    x: f64,
    y: f64,
    button: Option<MouseButton>,
    click_count: Option<i32>,
    buttons: i32,
) -> Result<()> {
    session
        .dispatch_mouse_event_full(crate::cdp::InputDispatchMouseEvent {
            r#type: event_type,
            x,
            y,
            button: button.map(MouseButton::cdp),
            click_count,
            buttons: (buttons != 0).then_some(buttons),
            force: Some(if buttons == 0 { 0.0 } else { 0.5 }),
            delta_x: None,
            delta_y: None,
        })
        .await
}

fn key_event(
    held_key: &HeldKey,
    event_type: crate::cdp::KeyEventType,
    modifiers: i32,
) -> crate::cdp::InputDispatchKeyEventFull {
    use crate::cdp::{
        modifiers::{ALT, CTRL, META},
        KeyEventType,
    };

    let key = key_for_modifiers(&held_key.key, modifiers);
    let text =
        if matches!(event_type, KeyEventType::KeyDown) && modifiers & (ALT | CTRL | META) == 0 {
            key_text(&key).map(str::to_owned)
        } else {
            None
        };
    let unmodified_text = text
        .as_ref()
        .and_then(|_| key_text(&held_key.key))
        .map(str::to_owned);
    crate::cdp::InputDispatchKeyEventFull {
        r#type: event_type,
        modifiers: (modifiers != 0).then_some(modifiers),
        key: Some(key),
        code: Some(held_key.code.clone()),
        text,
        unmodified_text,
        windows_virtual_key_code: held_key.virtual_key_code,
        native_virtual_key_code: held_key.virtual_key_code,
    }
}

async fn dispatch_coordinated_key_event(
    session: &Session,
    held_key: &HeldKey,
    event_type: crate::cdp::KeyEventType,
    modifiers: i32,
) -> Result<()> {
    session
        .dispatch_key_event_full(key_event(held_key, event_type, modifiers))
        .await
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
    /// Calling this twice for the same button returns [`Error::InputState`].
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
            if let Err(error) = self
                .dispatch_mouse_event(
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
            }
            state.finish_mouse_up(button);
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn dispatch_mouse_event(
        &self,
        event_type: MouseEventType,
        x: f64,
        y: f64,
        button: Option<MouseButton>,
        click_count: Option<i32>,
        buttons: i32,
    ) -> Result<()> {
        self.session
            .dispatch_mouse_event_full(crate::cdp::InputDispatchMouseEvent {
                r#type: event_type,
                x,
                y,
                button: button.map(MouseButton::cdp),
                click_count,
                buttons: (buttons != 0).then_some(buttons),
                force: Some(if buttons == 0 { 0.0 } else { 0.5 }),
                delta_x: None,
                delta_y: None,
            })
            .await
    }

    /// Type text into focused element
    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.session.insert_text(text).await
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
    pub async fn press_key(&self, key: &str) -> Result<()> {
        self.key_down(key).await?;
        sleep_ms(INTERACTION_DELAY_MS).await;
        self.key_up(key).await
    }

    /// Press and hold a key, optionally including modifiers in `key`.
    ///
    /// For example, `key_down("Ctrl+A")` dispatches `A` with the Control
    /// modifier. To hold a modifier across calls, use `key_down("Ctrl")`,
    /// then call `key_down("A")`. Printable key-downs can insert text; key-up and
    /// cleanup never insert text. A literal `"A"` means uppercase, while `"a"`
    /// means lowercase unless Shift is held. Duplicate key-down calls return
    /// [`Error::InputState`].
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
    pub async fn select_all(&self) -> Result<()> {
        self.press_key(if cfg!(target_os = "macos") {
            "Cmd+A"
        } else {
            "Ctrl+A"
        })
        .await
    }

    /// Platform-aware copy (Cmd+C on Mac, Ctrl+C elsewhere)
    pub async fn copy(&self) -> Result<()> {
        self.press_key(if cfg!(target_os = "macos") {
            "Cmd+C"
        } else {
            "Ctrl+C"
        })
        .await
    }

    /// Platform-aware paste (Cmd+V on Mac, Ctrl+V elsewhere)
    pub async fn paste(&self) -> Result<()> {
        self.press_key(if cfg!(target_os = "macos") {
            "Cmd+V"
        } else {
            "Ctrl+V"
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_payload_carries_text_only_on_unmodified_or_shift_key_down() {
        use crate::cdp::{
            modifiers::{ALT, CTRL, META, SHIFT},
            KeyEventType,
        };
        let (_, key) = held_key_from_combo("o");
        for modifiers in [0, SHIFT, CTRL, ALT, META, CTRL | SHIFT] {
            let down = key_event(&key, KeyEventType::KeyDown, modifiers);
            assert_eq!(
                down.key.as_deref(),
                Some(if modifiers & SHIFT != 0 { "O" } else { "o" })
            );
            let expected = match modifiers {
                0 => Some("o"),
                SHIFT => Some("O"),
                _ => None,
            };
            assert_eq!(down.text.as_deref(), expected);
            assert_eq!(down.unmodified_text.as_deref(), expected.map(|_| "o"));
            let up = key_event(&key, KeyEventType::KeyUp, modifiers);
            assert!(up.text.is_none() && up.unmodified_text.is_none());
        }
        for name in ["Tab", "Backspace", "ArrowLeft", "Shift", "Ctrl"] {
            let (_, key) = held_key_from_combo(name);
            assert!(key_event(&key, KeyEventType::KeyDown, 0).text.is_none());
        }
        let (_, enter) = held_key_from_combo("Enter");
        assert_eq!(
            key_event(&enter, KeyEventType::KeyDown, 0).text.as_deref(),
            Some("\r")
        );
    }

    #[test]
    fn mouse_reservation_is_included_in_its_dispatched_mask() {
        let mut state = HeldInputState::default();
        assert_eq!(
            state
                .reserve_mouse_down(MouseButton::Left, (10.0, 20.0))
                .unwrap(),
            1
        );
        // A concurrent transition observes Left before it is dispatched.
        assert_eq!(
            state
                .reserve_mouse_down(MouseButton::Right, (10.0, 20.0))
                .unwrap(),
            3
        );
        assert_eq!(state.mouse_up_mask(MouseButton::Right).unwrap(), 1);
        state.finish_mouse_up(MouseButton::Right);
        assert_eq!(state.mouse_button_mask(), 1);
        assert!(state
            .reserve_mouse_down(MouseButton::Left, (0.0, 0.0))
            .is_err());
    }

    #[test]
    fn cancelled_down_reservations_remain_cleanupable() {
        let mut state = HeldInputState::default();
        state
            .reserve_mouse_down(MouseButton::Left, (0.0, 0.0))
            .unwrap();
        let (ctrl_id, ctrl) = held_key_from_combo("Ctrl");
        state.reserve_key_down(ctrl_id.clone(), ctrl).unwrap();

        // Dropping an in-flight future skips its normal finish path, but the
        // reservation remains available to a later release_all_inputs call.
        assert_eq!(state.mouse_up_mask(MouseButton::Left).unwrap(), 0);
        assert!(state.key_up_event(&ctrl_id).is_ok());
    }

    #[tokio::test]
    async fn cancelling_a_reserved_transition_drops_the_lock_but_keeps_cleanup_state() {
        let state = Arc::new(tokio::sync::Mutex::new(HeldInputState::default()));
        let reserved = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let mut state = reserved.lock().await;
            state
                .reserve_mouse_down(MouseButton::Left, (0.0, 0.0))
                .unwrap();
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;

        // No pending/releasing flag survives cancellation, and cleanup still
        // has the reservation needed to dispatch MouseReleased.
        let state = state.lock().await;
        assert_eq!(state.mouse_up_mask(MouseButton::Left).unwrap(), 0);
    }

    #[tokio::test]
    async fn cancelling_release_keeps_input_available_for_a_later_cleanup() {
        let state = Arc::new(tokio::sync::Mutex::new(HeldInputState::default()));
        {
            let mut state = state.lock().await;
            state
                .reserve_mouse_down(MouseButton::Left, (0.0, 0.0))
                .unwrap();
        }
        let releasing = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let state = releasing.lock().await;
            assert_eq!(state.mouse_up_mask(MouseButton::Left).unwrap(), 0);
            // Model cancellation during the asynchronous CDP release.
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;

        let state = state.lock().await;
        assert_eq!(state.mouse_up_mask(MouseButton::Left).unwrap(), 0);
    }

    #[test]
    fn failed_mouse_down_is_removed_after_dispatch_error() {
        let mut state = HeldInputState::default();
        state
            .reserve_mouse_down(MouseButton::Right, (0.0, 0.0))
            .unwrap();
        state.cancel_mouse_down(MouseButton::Right);
        assert_eq!(state.mouse_button_mask(), 0);
        assert!(state.mouse_up_mask(MouseButton::Right).is_err());
    }

    #[test]
    fn held_modifier_is_applied_to_later_key_downs_and_removed_for_later_key_up() {
        let mut state = HeldInputState::default();
        let (ctrl_id, ctrl) = held_key_from_combo("Ctrl");
        assert_eq!(
            state.reserve_key_down(ctrl_id.clone(), ctrl).unwrap(),
            crate::cdp::modifiers::CTRL
        );

        let (a_id, a) = held_key_from_combo("A");
        assert_eq!(
            state.reserve_key_down(a_id.clone(), a).unwrap(),
            crate::cdp::modifiers::CTRL
        );
        assert_eq!(
            state.key_up_event(&ctrl_id).unwrap().1,
            crate::cdp::modifiers::CTRL
        );
        state.finish_key_up(&ctrl_id);
        assert_eq!(state.key_up_event(&a_id).unwrap().1, 0);
    }

    #[test]
    fn key_identity_is_case_insensitive_but_keeps_combo_modifiers() {
        let (lower, _) = held_key_from_combo("ctrl+a");
        let (upper, _) = held_key_from_combo("Ctrl+A");
        let (without_modifier, _) = held_key_from_combo("A");
        assert_eq!(lower, upper);
        assert_ne!(lower, without_modifier);
    }
}
