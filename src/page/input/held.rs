//! Held key and mouse-button state shared by every input path on a target.

use std::collections::HashMap;

use super::MouseButton;
use crate::error::Error;
use crate::keyboard::{key_to_codes, parse_key_combo};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct HeldKeyId {
    modifiers: i32,
    key: String,
}

#[derive(Debug, Clone)]
pub(super) struct HeldKey {
    pub(super) combo_modifiers: i32,
    pub(super) key: String,
    pub(super) code: String,
    pub(super) virtual_key_code: Option<i32>,
    pub(super) modifier_bit: i32,
}

#[derive(Default)]
pub(crate) struct HeldInputState {
    pub(super) mouse_buttons: HashMap<MouseButton, (f64, f64)>,
    pub(super) keys: HashMap<HeldKeyId, HeldKey>,
    /// Where the last mouse event was dispatched, so human paths start at the
    /// real pointer position instead of jumping in from nowhere.
    pub(super) pointer: Option<(f64, f64)>,
}

impl HeldInputState {
    /// Last dispatched pointer position, if any mouse event was sent.
    pub(crate) fn pointer(&self) -> Option<(f64, f64)> {
        self.pointer
    }
}

pub(super) fn input_state_error(message: impl Into<String>) -> Error {
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

pub(super) fn held_key_from_combo(combo: &str) -> (HeldKeyId, HeldKey) {
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
    pub(super) fn mouse_button_mask(&self) -> i32 {
        self.mouse_buttons
            .keys()
            .fold(0, |mask, button| mask | button.bit())
    }

    pub(super) fn active_key_modifiers(&self) -> i32 {
        self.keys
            .values()
            .fold(0, |modifiers, key| modifiers | key.modifier_bit)
    }

    pub(super) fn reserve_mouse_down(
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

    pub(super) fn cancel_mouse_down(&mut self, button: MouseButton) {
        self.mouse_buttons.remove(&button);
    }

    pub(super) fn mouse_up_mask(
        &self,
        button: MouseButton,
    ) -> std::result::Result<i32, &'static str> {
        if !self.mouse_buttons.contains_key(&button) {
            return Err("mouse button is not held");
        }
        Ok(self.mouse_button_mask() & !button.bit())
    }

    pub(super) fn finish_mouse_up(&mut self, button: MouseButton) {
        self.mouse_buttons.remove(&button);
    }

    pub(super) fn reserve_key_down(
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

    pub(super) fn cancel_key_down(&mut self, id: &HeldKeyId) {
        self.keys.remove(id);
    }

    pub(super) fn key_up_event(
        &self,
        id: &HeldKeyId,
    ) -> std::result::Result<(HeldKey, i32), &'static str> {
        let held_key = self.keys.get(id).cloned().ok_or("key is not held")?;
        // Read modifiers from the current held state rather than preserving
        // the key-down snapshot. The key being released contributes to its
        // own key-up event; subsequent releases observe its removal.
        let modifiers = self.active_key_modifiers() | held_key.combo_modifiers;
        Ok((held_key, modifiers))
    }

    pub(super) fn finish_key_up(&mut self, id: &HeldKeyId) {
        self.keys.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
