use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use super::{DispatchObserver, DispatchState};
use crate::{
    cdp::{transport::CommandDispatch, Session},
    Result,
};
use serde_json::Value;

pub(crate) struct Dispatch {
    observer: Option<Arc<dyn DispatchObserver>>,
    tokens: Mutex<Vec<(Arc<CommandDispatch>, Arc<AtomicBool>)>>,
    preparation: AtomicBool,
}

impl Dispatch {
    pub fn new(observer: Option<Arc<dyn DispatchObserver>>) -> Self {
        Self {
            observer,
            tokens: Mutex::new(Vec::new()),
            preparation: AtomicBool::new(false),
        }
    }

    pub fn state(&self) -> DispatchState {
        let tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if !tokens.iter().any(|(token, _)| token.may_have_been_sent()) {
            DispatchState::NotSent
        } else if tokens
            .iter()
            .any(|(token, ack)| token.may_have_been_sent() && !ack.load(Ordering::Acquire))
        {
            DispatchState::MayHaveBeenSent
        } else {
            DispatchState::Acknowledged
        }
    }

    pub fn prepared(&self) -> bool {
        self.preparation.load(Ordering::Acquire)
    }

    pub async fn send(
        &self,
        session: &Session,
        method: &str,
        params: Value,
        preparation: bool,
    ) -> Result<Value> {
        if let Some(observer) = &self.observer {
            observer.before_dispatch(preparation).await?;
        }
        let token = Arc::new(CommandDispatch::new());
        let acknowledged = Arc::new(AtomicBool::new(false));
        if preparation {
            self.preparation.store(true, Ordering::Release);
        } else {
            self.tokens
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((token.clone(), acknowledged.clone()));
        }
        let result = session.send_tracked(method, &params, &token).await;
        if result.is_ok() {
            acknowledged.store(true, Ordering::Release);
        }
        result
    }
}
