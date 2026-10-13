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
                json!({"type":"mouseMoved","x":x,"y":y,"buttons":0}),
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
        dispatch.send(&self.session, "Input.dispatchMouseEvent", json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":buttons,"clickCount":count}), false).await?;
        if human {
            sleep_ms(50 + fastrand::u64(0..70)).await;
        }
        dispatch.send(&self.session, "Input.dispatchMouseEvent", json!({"type":"mouseReleased","x":x,"y":y,"button":"left","buttons":0,"clickCount":count}), false).await?;
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

    pub async fn text(&mut self, dispatch: &Dispatch, text: &str, human: bool) -> Result<()> {
        if human && !text.is_empty() {
            for ch in text.chars() {
                dispatch
                    .send(
                        &self.session,
                        "Input.insertText",
                        json!({"text":ch.to_string()}),
                        false,
                    )
                    .await?;
                sleep_ms(30 + fastrand::u64(0..60)).await;
            }
        } else {
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
        dispatch.send(&self.session,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1}),false).await?;
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
                    json!({"type":"mouseMoved","x":px,"y":py,"buttons":1}),
                    false,
                )
                .await?;
            sleep_ms(8).await;
        }
        dispatch.send(&self.session,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":x+dx,"y":y+dy,"button":"left","buttons":0,"clickCount":1}),false).await?;
        self.state.finish_mouse_up(MouseButton::Left);
        Ok(())
    }

    pub async fn cleanup(&mut self) -> Result<()> {
        for id in self.state.keys.keys().cloned().collect::<Vec<_>>() {
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
        for (button, (x, y)) in self.state.mouse_buttons.clone() {
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
