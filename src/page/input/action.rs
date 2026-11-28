use super::*;
use crate::action::dispatch::Dispatch;
use serde_json::json;
use tokio::sync::OwnedMutexGuard;

pub(crate) struct ActionInput {
    session: Session,
    state: OwnedMutexGuard<HeldInputState>,
}

impl ActionInput {
    pub fn has_held_input(&self) -> bool {
        !self.state.keys.is_empty() || !self.state.mouse_buttons.is_empty()
    }

    pub fn discard_unsent_reservations(&mut self) {
        self.state.keys.clear();
        self.state.mouse_buttons.clear();
    }

    pub async fn acquire(session: &Session) -> Result<Self> {
        let state = session.held_input().lock_owned().await;
        if !state.mouse_buttons.is_empty() || !state.keys.is_empty() {
            return Err(input_state_error(
                "Release held input before a high-level action",
            ));
        }
        Ok(Self {
            session: session.clone(),
            state,
        })
    }

    pub async fn move_to(&mut self, dispatch: &Dispatch, x: f64, y: f64) -> Result<()> {
        dispatch
            .send(
                &self.session,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":x,"y":y,"buttons":0,"force":0.0}),
                true,
            )
            .await?;
        Ok(())
    }

    pub async fn human_move(&mut self, dispatch: &Dispatch, x: f64, y: f64) -> Result<()> {
        let start = (
            (x + fastrand::f64() * 400.0 - 200.0).max(0.0),
            (y + fastrand::f64() * 200.0 - 100.0).max(0.0),
        );
        for (px, py) in crate::stealth::human::bezier_curve(start, (x, y), 24) {
            self.move_to(dispatch, px, py).await?;
            sleep_ms(5 + fastrand::u64(0..8)).await;
        }
        Ok(())
    }

    pub async fn click(
        &mut self,
        dispatch: &Dispatch,
        x: f64,
        y: f64,
        count: i32,
        human: bool,
    ) -> Result<()> {
        let buttons = self
            .state
            .reserve_mouse_down(MouseButton::Left, (x, y))
            .map_err(input_state_error)?;
        dispatch.send(&self.session, "Input.dispatchMouseEvent", json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":buttons,"clickCount":count,"force":0.5}), false).await?;
        if human {
            sleep_ms(50 + fastrand::u64(0..70)).await;
        }
        dispatch.send(&self.session, "Input.dispatchMouseEvent", json!({"type":"mouseReleased","x":x,"y":y,"button":"left","buttons":0,"clickCount":count,"force":0.0}), false).await?;
        self.state.finish_mouse_up(MouseButton::Left);
        Ok(())
    }

    pub async fn key(&mut self, dispatch: &Dispatch, key: &str) -> Result<()> {
        let (id, held) = held_key_from_combo(key);
        let modifiers = self
            .state
            .reserve_key_down(id.clone(), held.clone())
            .map_err(input_state_error)?
            | held.combo_modifiers;
        dispatch
            .send(
                &self.session,
                "Input.dispatchKeyEvent",
                serde_json::to_value(key_event(
                    &held,
                    crate::cdp::KeyEventType::KeyDown,
                    modifiers,
                ))?,
                false,
            )
            .await?;
        dispatch
            .send(
                &self.session,
                "Input.dispatchKeyEvent",
                serde_json::to_value(key_event(&held, crate::cdp::KeyEventType::KeyUp, modifiers))?,
                false,
            )
            .await?;
        self.state.finish_key_up(&id);
        Ok(())
    }

    pub async fn text<F: std::future::Future<Output = Result<()>>>(
        &mut self,
        dispatch: &Dispatch,
        text: &str,
        mode: crate::InputMode,
        mut before_insert: impl FnMut() -> F,
    ) -> Result<()> {
        if mode == crate::InputMode::Human && !text.is_empty() {
            for ch in text.chars() {
                before_insert().await?;
                let key = ch.to_string();
                let (code, virtual_key, shift) = us_key(ch);
                let modifiers = if shift { 8 } else { 0 };
                dispatch
                    .send(
                        &self.session,
                        "Input.dispatchKeyEvent",
                        json!({"type":"keyDown","key":key,"code":code,"text":key,"unmodifiedText":key,
                            "windowsVirtualKeyCode":virtual_key,"nativeVirtualKeyCode":virtual_key,"modifiers":modifiers}),
                        false,
                    )
                    .await?;
                sleep_ms(fastrand::u64(15..45)).await;
                dispatch
                    .send(
                        &self.session,
                        "Input.dispatchKeyEvent",
                        json!({"type":"keyUp","key":key,"code":code,
                            "windowsVirtualKeyCode":virtual_key,"nativeVirtualKeyCode":virtual_key,"modifiers":modifiers}),
                        false,
                    )
                    .await?;
                let pause = if ch == ' ' || ch.is_ascii_punctuation() {
                    fastrand::u64(90..220)
                } else {
                    fastrand::u64(50..150)
                };
                sleep_ms(pause).await;
            }
        } else if mode == crate::InputMode::Sequential && !text.is_empty() {
            for ch in text.chars() {
                before_insert().await?;
                dispatch
                    .send(
                        &self.session,
                        "Input.insertText",
                        json!({"text":ch.to_string()}),
                        false,
                    )
                    .await?;
                sleep_ms(30).await;
            }
        } else {
            before_insert().await?;
            dispatch
                .send(
                    &self.session,
                    "Input.insertText",
                    json!({"text":text}),
                    false,
                )
                .await?;
        }
        Ok(())
    }

    pub async fn drag(
        &mut self,
        dispatch: &Dispatch,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    ) -> Result<()> {
        self.move_to(dispatch, x, y).await?;
        self.state
            .reserve_mouse_down(MouseButton::Left, (x, y))
            .map_err(input_state_error)?;
        dispatch.send(&self.session,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1,"force":0.5}),false).await?;
        for step in 1..=24 {
            let t = f64::from(step) / 24.0;
            let shifted = t - 1.0;
            let t = 1.0 + 2.70158 * shifted.powi(3) + 1.70158 * shifted.powi(2);
            let (px, py) = (x + dx * t, y + dy * t);
            self.state.mouse_buttons.insert(MouseButton::Left, (px, py));
            dispatch
                .send(
                    &self.session,
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseMoved","x":px,"y":py,"buttons":1,"force":0.5}),
                    false,
                )
                .await?;
            sleep_ms(8).await;
        }
        dispatch.send(&self.session,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":x+dx,"y":y+dy,"button":"left","buttons":0,"clickCount":1,"force":0.0}),false).await?;
        self.state.finish_mouse_up(MouseButton::Left);
        Ok(())
    }

    pub async fn cleanup(&mut self) -> Result<()> {
        while let Some(id) = self.state.keys.keys().next().cloned() {
            let (key, modifiers) = self.state.key_up_event(&id).map_err(input_state_error)?;
            dispatch_coordinated_key_event(
                &self.session,
                &key,
                crate::cdp::KeyEventType::KeyUp,
                modifiers,
            )
            .await?;
            self.state.finish_key_up(&id);
        }
        while let Some((&button, &(x, y))) = self.state.mouse_buttons.iter().next() {
            let buttons = self
                .state
                .mouse_up_mask(button)
                .map_err(input_state_error)?;
            dispatch_coordinated_mouse_event(
                &self.session,
                MouseEventType::MouseReleased,
                x,
                y,
                Some(button),
                Some(1),
                buttons,
            )
            .await?;
            self.state.finish_mouse_up(button);
        }
        Ok(())
    }
}

/// US-layout `code`, Windows virtual key and whether Shift is needed for a
/// character. Characters without a US key get an empty code and key 0; Chrome
/// still inserts their `text`.
fn us_key(ch: char) -> (String, u32, bool) {
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    const PUNCTUATION: &[(char, char, &str, u32)] = &[
        ('-', '_', "Minus", 189),
        ('=', '+', "Equal", 187),
        ('[', '{', "BracketLeft", 219),
        (']', '}', "BracketRight", 221),
        ('\\', '|', "Backslash", 220),
        (';', ':', "Semicolon", 186),
        ('\'', '"', "Quote", 222),
        (',', '<', "Comma", 188),
        ('.', '>', "Period", 190),
        ('/', '?', "Slash", 191),
        ('`', '~', "Backquote", 192),
    ];
    if ch.is_ascii_alphabetic() {
        let upper = ch.to_ascii_uppercase();
        return (format!("Key{upper}"), upper as u32, ch.is_ascii_uppercase());
    }
    if let Some(digit) = ch.to_digit(10) {
        return (format!("Digit{digit}"), 48 + digit, false);
    }
    if let Some(index) = SHIFTED_DIGITS.find(ch) {
        return (format!("Digit{index}"), 48 + index as u32, true);
    }
    if ch == ' ' {
        return ("Space".into(), 32, false);
    }
    for &(plain, shifted, code, virtual_key) in PUNCTUATION {
        if ch == plain || ch == shifted {
            return (code.into(), virtual_key, ch == shifted);
        }
    }
    (String::new(), 0, false)
}

#[cfg(test)]
mod us_key_tests {
    use super::us_key;

    #[test]
    fn maps_us_layout_characters() {
        assert_eq!(us_key('a'), ("KeyA".into(), 65, false));
        assert_eq!(us_key('A'), ("KeyA".into(), 65, true));
        assert_eq!(us_key('7'), ("Digit7".into(), 55, false));
        assert_eq!(us_key('@'), ("Digit2".into(), 50, true));
        assert_eq!(us_key(' '), ("Space".into(), 32, false));
        assert_eq!(us_key('?'), ("Slash".into(), 191, true));
        assert_eq!(us_key('é'), (String::new(), 0, false));
    }
}
