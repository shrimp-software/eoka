use tokio::sync::OwnedMutexGuard;

use super::{dispatch_coordinated_mouse_event, input_state_error, HeldInputState};
use crate::cdp::{MouseEventType, Session};
use crate::error::Result;
use crate::page::MouseButton;

pub(crate) struct DragInput {
    session: Session,
    state: OwnedMutexGuard<HeldInputState>,
}

impl DragInput {
    pub(crate) async fn acquire(session: &Session) -> Result<Self> {
        let state = session.held_input().lock_owned().await;
        if state.mouse_buttons.contains_key(&MouseButton::Left) {
            return Err(input_state_error("mouse button is already held"));
        }
        Ok(Self {
            session: session.clone(),
            state,
        })
    }

    pub(crate) async fn move_to(&mut self, x: f64, y: f64) -> Result<()> {
        dispatch_coordinated_mouse_event(
            &self.session,
            MouseEventType::MouseMoved,
            x,
            y,
            None,
            None,
            self.state.mouse_button_mask(),
        )
        .await?;
        for position in self.state.mouse_buttons.values_mut() {
            *position = (x, y);
        }
        Ok(())
    }

    pub(crate) async fn press(&mut self, x: f64, y: f64) -> Result<()> {
        let buttons = self
            .state
            .reserve_mouse_down(MouseButton::Left, (x, y))
            .map_err(input_state_error)?;
        dispatch_coordinated_mouse_event(
            &self.session,
            MouseEventType::MousePressed,
            x,
            y,
            Some(MouseButton::Left),
            Some(1),
            buttons,
        )
        .await
    }

    pub(crate) async fn release(&mut self, x: f64, y: f64) -> Result<()> {
        let buttons = self
            .state
            .mouse_up_mask(MouseButton::Left)
            .map_err(input_state_error)?;
        let result = dispatch_coordinated_mouse_event(
            &self.session,
            MouseEventType::MouseReleased,
            x,
            y,
            Some(MouseButton::Left),
            Some(1),
            buttons,
        )
        .await;
        self.state.finish_mouse_up(MouseButton::Left);
        result
    }
}
