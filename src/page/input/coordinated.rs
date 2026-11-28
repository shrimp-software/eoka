//! CDP mouse and key dispatch through the held-input coordinator.

use std::sync::Arc;

use super::held::{held_key_from_combo, input_state_error, HeldInputState, HeldKey};
use super::MouseButton;
use crate::cdp::{MouseEventType, Session};
use crate::error::Result;
use crate::keyboard::{key_for_modifiers, key_text};

pub(crate) async fn held_mouse_move(
    session: &Session,
    state: &mut HeldInputState,
    x: f64,
    y: f64,
) -> Result<()> {
    dispatch_coordinated_mouse_event(
        session,
        MouseEventType::MouseMoved,
        x,
        y,
        None,
        None,
        state.mouse_button_mask(),
    )
    .await?;
    state.pointer = Some((x, y));
    for position in state.mouse_buttons.values_mut() {
        *position = (x, y);
    }
    Ok(())
}

pub(crate) async fn held_mouse_down(
    session: &Session,
    state: &mut HeldInputState,
    x: f64,
    y: f64,
    button: MouseButton,
) -> Result<()> {
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
    match result {
        Ok(()) => state.pointer = Some((x, y)),
        Err(_) => state.cancel_mouse_down(button),
    }
    result
}

pub(crate) async fn held_mouse_up(
    session: &Session,
    state: &mut HeldInputState,
    x: f64,
    y: f64,
    button: MouseButton,
) -> Result<()> {
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
    if result.is_ok() {
        state.pointer = Some((x, y));
    }
    result
}

pub(crate) async fn coordinated_mouse_move(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    x: f64,
    y: f64,
) -> Result<()> {
    held_mouse_move(session, &mut *held_input.lock().await, x, y).await
}

pub(crate) async fn coordinated_mouse_down(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    x: f64,
    y: f64,
    button: MouseButton,
) -> Result<()> {
    held_mouse_down(session, &mut *held_input.lock().await, x, y, button).await
}

pub(crate) async fn coordinated_mouse_up(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    x: f64,
    y: f64,
    button: MouseButton,
) -> Result<()> {
    held_mouse_up(session, &mut *held_input.lock().await, x, y, button).await
}

pub(crate) async fn coordinated_mouse_wheel(
    session: &Session,
    held_input: &Arc<tokio::sync::Mutex<HeldInputState>>,
    x: f64,
    y: f64,
    delta_x: f64,
    delta_y: f64,
) -> Result<()> {
    let mut state = held_input.lock().await;
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
        .await?;
    state.pointer = Some((x, y));
    Ok(())
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

pub(super) async fn dispatch_coordinated_mouse_event(
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

pub(super) fn key_event(
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

pub(super) async fn dispatch_coordinated_key_event(
    session: &Session,
    held_key: &HeldKey,
    event_type: crate::cdp::KeyEventType,
    modifiers: i32,
) -> Result<()> {
    session
        .dispatch_key_event_full(key_event(held_key, event_type, modifiers))
        .await
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
}
