//! Per-call builders behind the shorthand input methods such as `Page::click`.

use std::{future::Future, future::IntoFuture, pin::Pin};

use super::{Action, ActionOptions, ActionReceipt, Destination, Expectation};
use crate::{DispatchState, Error, Page, Result};

type ActionFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;

/// One shorthand input action, run when awaited.
///
/// Without an expectation, a successful `.await` proves only that the input was
/// dispatched to an actionable target: the receipt's outcome is
/// [`OutcomeUnknown`](crate::ActionOutcome::OutcomeUnknown). Use
/// [`expecting`](Self::expecting) to prove the page reacted, or
/// [`Page::act`] to require that proof. Actions are never replayed after dispatch.
#[must_use = "actions do nothing until awaited"]
pub struct ActionCall {
    page: Page,
    action: Action,
    options: ActionOptions,
}

impl ActionCall {
    pub(crate) fn new(page: &Page, action: Action) -> Self {
        Self {
            page: page.clone(),
            action,
            options: page.action_defaults.options(),
        }
    }

    /// Require this condition to become true after dispatch, for this call only.
    /// It must not already hold, so that it proves a new result.
    pub fn expecting(mut self, expect: Expectation) -> Self {
        self.options.expect = Some(expect);
        self
    }

    /// Set the total deadline in milliseconds, including queueing and verification.
    pub fn timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.options.timeout_ms = timeout_ms;
        self
    }

    /// Set where the expectation may be observed. Does not change the input target.
    pub fn destination(mut self, destination: Destination) -> Self {
        self.options.destination = destination;
        self
    }
}

impl IntoFuture for ActionCall {
    type Output = Result<ActionReceipt>;
    type IntoFuture = ActionFuture<ActionReceipt>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.page.explore(self.action, self.options).await })
    }
}

/// An [`ActionCall`] that skips an unavailable target instead of waiting for it.
///
/// Resolves to `Ok(false)` only when the target is missing or not actionable and
/// nothing was dispatched. `Ok(true)` means the input was dispatched; it proves
/// the page reacted only when combined with [`expecting`](Self::expecting).
#[must_use = "actions do nothing until awaited"]
pub struct TryActionCall(ActionCall);

impl TryActionCall {
    pub(crate) fn new(page: &Page, action: Action) -> Self {
        Self(ActionCall::new(page, action))
    }

    /// Require this condition to become true after dispatch, for this call only.
    /// It must not already hold, so that it proves a new result.
    pub fn expecting(self, expect: Expectation) -> Self {
        Self(self.0.expecting(expect))
    }

    /// Set the total deadline in milliseconds, including queueing and verification.
    pub fn timeout_ms(self, timeout_ms: u64) -> Self {
        Self(self.0.timeout_ms(timeout_ms))
    }

    /// Set where the expectation may be observed. Does not change the input target.
    pub fn destination(self, destination: Destination) -> Self {
        Self(self.0.destination(destination))
    }
}

impl IntoFuture for TryActionCall {
    type Output = Result<bool>;
    type IntoFuture = ActionFuture<bool>;

    fn into_future(self) -> Self::IntoFuture {
        let ActionCall {
            page,
            action,
            mut options,
        } = self.0;
        options.wait_for_target = false;
        Box::pin(async move {
            match page.explore(action, options).await {
                Ok(_) => Ok(true),
                Err(Error::Action {
                    receipt, source, ..
                }) if receipt.dispatch == DispatchState::NotSent
                    && matches!(
                        *source,
                        Error::ElementNotFound(_) | Error::ElementNotVisible { .. }
                    ) =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        })
    }
}
