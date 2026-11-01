use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use serde_json::Value;
use tokio::sync::OwnedMutexGuard;

use super::{
    dispatch::Dispatch,
    locator::resolve,
    perform::{self, Verification},
    *,
};
use crate::{cdp::transport::CdpMessage, page::ActionInput, Error, Page, Result};

struct Cleanup {
    gate: Option<OwnedMutexGuard<()>>,
    input: Option<ActionInput>,
    state: Arc<ActionState>,
    dispatch: Arc<Dispatch>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let Some(mut input) = self.input.take() else {
            return;
        };
        if self.dispatch.state() == DispatchState::NotSent {
            input.discard_unsent_reservations();
        }
        if !input.has_held_input() {
            return;
        }
        let gate = self.gate.take();
        let state = self.state.clone();
        state.cleanup_failed.store(true, Ordering::Release);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let succeeded = matches!(
                    tokio::time::timeout(Duration::from_secs(5), input.cleanup()).await,
                    Ok(Ok(()))
                );
                state.cleanup_failed.store(!succeeded, Ordering::Release);
                drop(input);
                drop(gate);
            });
        }
    }
}

impl Page {
    /// Resolve, prepare and execute an action once, then verify its result.
    /// The returned error retains a receipt when completion is uncertain.
    pub async fn act(&self, action: Action, options: ActionOptions) -> Result<ActionReceipt> {
        self.run_action(action, options, false).await
    }

    /// Execute one exploratory action with the same actionability and deadline checks.
    /// Without intrinsic or caller-supplied completion evidence, successful dispatch
    /// returns `Ok` with `ActionOutcome::OutcomeUnknown`, never verified completion.
    pub async fn explore(&self, action: Action, options: ActionOptions) -> Result<ActionReceipt> {
        self.run_action(action, options, true).await
    }

    async fn run_action(
        &self,
        action: Action,
        options: ActionOptions,
        exploratory: bool,
    ) -> Result<ActionReceipt> {
        let started = tokio::time::Instant::now();
        let dispatch = Arc::new(Dispatch::new(options.observer.clone()));
        let mut receipt = ActionReceipt {
            destination_target_id: None,
            started_at_ms: timestamp(),
            finished_at_ms: 0,
            download_guid: None,
            action: action.kind().into(),
            target_id: self.target_id().into(),
            outcome: ActionOutcome::NotDispatched,
            dispatch: DispatchState::NotSent,
            preparation_may_have_executed: false,
            verification: None,
            elapsed_ms: 0,
            already_satisfied: false,
            cleanup_pending: false,
        };
        let state = self.session().action_state();
        let mut cleanup = Cleanup {
            gate: None,
            input: None,
            state: state.clone(),
            dispatch: dispatch.clone(),
        };
        let execution = async {
            if options.timeout_ms == 0 || options.timeout_ms > 86_400_000 {
                return Err(Error::cdp_msg(
                    "invalid_timeout: action timeout must be between 1 and 86400000ms",
                ));
            }
            if action.requires_expectation() && options.expect.is_none() && !exploratory {
                return Err(Error::cdp_msg(
                    "expectation_required: action requires an expected result",
                ));
            }
            cleanup.gate = Some(state.gate.clone().lock_owned().await);
            if state.cleanup_failed.load(Ordering::Acquire) {
                return Err(Error::InputState(
                    "Previous action input cleanup failed; release_all_inputs is required".into(),
                ));
            }
            if let Some(expect) = &options.expect {
                validate_expectation(expect, 0)?;
            }
            if let Some(expect) = &options.expect {
                if options.destination != Destination::NewTab
                    && self.check_expectation(expect).await?
                {
                    return Err(Error::cdp_msg("expectation_already_satisfied: choose a condition that proves a new result"));
                }
            }
            let mut destination_watch = if options.destination != Destination::Current
                && options
                    .expect
                    .as_ref()
                    .is_some_and(|e| !matches!(e, Expectation::DownloadStarted))
            {
                Some(super::destination::DestinationWatch::begin(self, options.destination).await?)
            } else {
                None
            };
            dispatch.note("preparation");
            cleanup.input = Some(ActionInput::acquire(self.session()).await?);
            let download_frames = if matches!(options.expect, Some(Expectation::DownloadStarted)) {
                self.frames()
                    .await?
                    .into_iter()
                    .map(|f| f.id)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let main_frame = self.session().get_frame_tree().await?.frame.id;
            let mut events = self.session().transport().subscribe();
            let (node, verification) = loop {
                let attempt = async {
                    let mut node =
                        perform::prepare(self, &action, &dispatch, options.wait_for_target).await?;
                    if matches!(action, Action::Hover { .. }) {
                        if let Some(node) = node.as_mut() {
                            node.arm_hover().await?;
                        }
                    }
                    let verification = perform::perform(
                        self,
                        &action,
                        node.as_ref(),
                        cleanup
                            .input
                            .as_mut()
                            .ok_or_else(|| Error::cdp_msg("Action input missing"))?,
                        &dispatch,
                    )
                    .await?;

                    Ok((node, verification))
                }
                .await;
                match attempt {
                    Ok(result) => break result,
                    Err(error)
                        if options.wait_for_target
                            && dispatch.state() == DispatchState::NotSent
                            && perform::retryable_preparation(&action, &error) =>
                    {
                        perform::pause().await;
                    }
                    Err(error) => return Err(error),
                }
            };

            receipt.already_satisfied = matches!(verification, Verification::Noop);
            dispatch.note("verification");
            match verification {
                Verification::Properties { field, expected } => {
                    loop {
                        if let Some(node) = node.as_ref() {
                            let properties = node.properties().await?;
                            if properties["connected"] != true {
                                return Err(Error::cdp_msg("Target detached after dispatch"));
                            }
                            let mut actual = properties[field].clone();
                            if field == "selected" {
                                if let Some(values) = actual.as_array_mut() {
                                    values.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                                }
                            }
                            if actual == expected {
                                break;
                            }
                        }
                        perform::pause().await;
                    }
                    receipt.verification = Some(field.into());
                }
                Verification::Navigation { .. }
                    if matches!(options.expect, Some(Expectation::DownloadStarted)) => {}
                Verification::Navigation { loader } => {
                    loop {
                        let event = events.recv().await.map_err(|error| {
                            Error::transport_io(
                                "Navigation event stream lost",
                                std::io::Error::other(error),
                            )
                        })?;
                        if let CdpMessage::Event {
                            method,
                            params,
                            session_id,
                        } = event
                        {
                            if session_id.as_deref() != Some(self.session().session_id()) {
                                continue;
                            }
                            if (method == "Page.frameNavigated"
                                && params["frame"]["parentId"].is_null()
                                && loader
                                    .as_ref()
                                    .is_none_or(|id| params["frame"]["loaderId"] == *id))
                                || (method == "Page.navigatedWithinDocument"
                                    && loader.is_none()
                                    && params["frameId"] == main_frame)
                            {
                                break;
                            }
                        }
                    }
                    self.invalidate_document();
                    while !self.check_expectation(&Expectation::DocumentReady).await? {
                        perform::pause().await;
                    }
                    receipt.verification = Some("document_ready".into());
                }
                Verification::Noop => {
                    receipt.verification = Some("already_satisfied".into());
                }
                Verification::Explicit => {}
            }
            if let Some(Expectation::DownloadStarted) = &options.expect {
                loop {
                    match events.recv().await.map_err(|error| {
                        Error::transport_io(
                            "Download event stream lost",
                            std::io::Error::other(error),
                        )
                    })? {
                        CdpMessage::Event { method, params, .. }
                            if method == "Browser.downloadWillBegin"
                                && params["frameId"]
                                    .as_str()
                                    .is_some_and(|id| download_frames.iter().any(|f| f == id)) =>
                        {
                            receipt.download_guid = params["guid"].as_str().map(str::to_owned);
                            receipt.verification = Some("download_started".into());
                            break;
                        }
                        _ => {}
                    }
                }
            } else if let Some(expect) = &options.expect {
                loop {
                    let target = if let Some(watch) = destination_watch.as_mut() {
                        watch.check(expect).await?
                    } else if self.check_expectation(expect).await? {
                        Some(self.target_id().into())
                    } else {
                        None
                    };
                    if let Some(target) = target {
                        receipt.destination_target_id = Some(target);
                        break;
                    }
                    perform::pause().await;
                }
                receipt.verification = Some(expect.kind().into());
            }
            cleanup
                .input
                .as_mut()
                .ok_or_else(|| Error::cdp_msg("Action input missing"))?
                .cleanup()
                .await?;
            cleanup.input.take();
            receipt.outcome = if receipt.verification.is_some() {
                ActionOutcome::VerifiedComplete
            } else {
                ActionOutcome::OutcomeUnknown
            };
            Ok(())
        };
        let result = tokio::time::timeout(
            Duration::from_millis(options.timeout_ms.min(86_400_000)),
            execution,
        )
        .await
        .unwrap_or_else(|_| {
            Err(Error::Timeout(format!(
                "Action deadline exceeded during {}",
                dispatch.context()
            )))
        });
        receipt.finished_at_ms = timestamp();
        receipt.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        receipt.dispatch = dispatch.state();
        receipt.preparation_may_have_executed = dispatch.prepared();
        match result {
            Ok(()) => Ok(receipt),
            Err(source) => {
                receipt.outcome = if receipt.dispatch == DispatchState::NotSent {
                    ActionOutcome::NotDispatched
                } else {
                    ActionOutcome::OutcomeUnknown
                };
                receipt.cleanup_pending = dispatch.state() != DispatchState::NotSent
                    && cleanup
                        .input
                        .as_ref()
                        .is_some_and(ActionInput::has_held_input);
                let code = match &source {
                    Error::Timeout(_) => "action_timeout",
                    Error::InputState(_) => "input_state",
                    Error::ElementNotFound(_) | Error::ElementNotVisible { .. } => {
                        "target_unavailable"
                    }
                    Error::Cdp { message, .. } if message.starts_with("ambiguous_target:") => {
                        "ambiguous_target"
                    }
                    Error::Cdp { message, .. } if message.starts_with("expectation_required:") => {
                        "expectation_required"
                    }
                    Error::Cdp { message, .. }
                        if message.starts_with("expectation_already_satisfied:") =>
                    {
                        "expectation_already_satisfied"
                    }
                    _ => "action_failed",
                }
                .into();
                Err(Error::Action {
                    code,
                    receipt: Box::new(receipt),
                    source: Box::new(source),
                })
            }
        }
    }

    pub(super) async fn check_expectation(&self, expect: &Expectation) -> Result<bool> {
        let (target, field, expected, contains) = match expect {
            Expectation::DocumentReady => return self
                .evaluate_sync(
                    "document.readyState === 'interactive' || document.readyState === 'complete'",
                )
                .await,
            Expectation::All { conditions } => {
                if conditions.is_empty() {
                    return Err(Error::cdp_msg(
                        "invalid_expectation: all requires at least one condition",
                    ));
                }
                for condition in conditions {
                    if !Box::pin(self.check_expectation(condition)).await? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            Expectation::Enabled { target, enabled } => {
                (target, "disabled", Value::Bool(!enabled), false)
            }
            Expectation::DownloadStarted => return Ok(false),
            Expectation::Focused { target } => (target, "focused", Value::Bool(true), false),
            Expectation::Url { value, contains } => {
                let url = self.url().await?;
                return Ok(if *contains {
                    url.contains(value)
                } else {
                    url == *value
                });
            }
            Expectation::Visible { target } => (target, "visible", Value::Bool(true), false),
            Expectation::Hidden { target } => (target, "visible", Value::Bool(false), false),
            Expectation::Text {
                target,
                value,
                contains,
            } => (target, "text", Value::String(value.clone()), *contains),
            Expectation::Value { target, value } => {
                (target, "value", Value::String(value.clone()), false)
            }
            Expectation::Checked { target, checked } => {
                (target, "checked", Value::Bool(*checked), false)
            }
            Expectation::Selected { target, values } => {
                let mut values = values.clone();
                values.sort();
                values.dedup();
                (target, "selected", serde_json::json!(values), false)
            }
        };
        let Some(node) = resolve(self, target).await? else {
            return Ok(matches!(expect, Expectation::Hidden { .. }));
        };
        let properties = node.properties().await?;
        if matches!(expect, Expectation::Enabled { .. }) && properties["visible"] != true {
            return Ok(false);
        }
        let mut actual = properties[field].clone();
        if field == "selected" {
            if let Some(values) = actual.as_array_mut() {
                values.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
        }
        Ok(if contains {
            actual
                .as_str()
                .is_some_and(|text| text.contains(expected.as_str().unwrap_or_default()))
        } else {
            actual == expected
        })
    }

    /// Wait for observable page state without dispatching input or replaying an action.
    pub async fn wait_for_expectation(
        &self,
        expectation: &Expectation,
        timeout_ms: u64,
    ) -> Result<()> {
        validate_expectation(expectation, 0)?;
        if timeout_ms == 0
            || timeout_ms > 86_400_000
            || matches!(expectation, Expectation::DownloadStarted)
        {
            return Err(Error::cdp_msg("Invalid observation wait"));
        }
        tokio::time::timeout(Duration::from_millis(timeout_ms), async {
            while !self.check_expectation(expectation).await? {
                perform::pause().await;
            }
            Ok(())
        })
        .await
        .map_err(|_| Error::Timeout("Page expectation was not satisfied".into()))?
    }
}

fn validate_expectation(expectation: &Expectation, depth: usize) -> Result<()> {
    if depth > 8 {
        return Err(Error::cdp_msg("invalid_expectation: nesting exceeds 8"));
    }
    match expectation {
        Expectation::All { conditions } => {
            if conditions.is_empty() || conditions.len() > 16 {
                return Err(Error::cdp_msg(
                    "invalid_expectation: all requires 1 through 16 conditions",
                ));
            }
            for condition in conditions {
                validate_expectation(condition, depth + 1)?;
            }
        }
        Expectation::DownloadStarted if depth > 0 => {
            return Err(Error::cdp_msg(
                "invalid_expectation: download_started must be a standalone event expectation",
            ))
        }
        _ => {}
    }
    Ok(())
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}
