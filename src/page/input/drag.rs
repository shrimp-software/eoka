use tokio::sync::OwnedMutexGuard;

use super::{held_mouse_down, held_mouse_move, held_mouse_up, input_state_error, HeldInputState};
use crate::cdp::Session;
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
        held_mouse_move(&self.session, &mut self.state, x, y).await
    }

    pub(crate) async fn press(&mut self, x: f64, y: f64) -> Result<()> {
        held_mouse_down(&self.session, &mut self.state, x, y, MouseButton::Left).await
    }

    pub(crate) async fn release(&mut self, x: f64, y: f64) -> Result<()> {
        held_mouse_up(&self.session, &mut self.state, x, y, MouseButton::Left).await
    }
}
