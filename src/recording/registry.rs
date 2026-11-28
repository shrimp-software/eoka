use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use super::*;

pub(crate) type RecordingSlot = Arc<Mutex<Option<Arc<RecordingState>>>>;
type RecordingRegistry = HashMap<String, HashMap<String, Weak<Mutex<Option<Arc<RecordingState>>>>>>;

pub(crate) fn recording_for_target(endpoint: Option<&str>, target_id: &str) -> RecordingSlot {
    static REGISTRY: LazyLock<Mutex<RecordingRegistry>> = LazyLock::new(Mutex::default);
    let Some(endpoint) = endpoint else {
        return Arc::default();
    };
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = registry
        .get(endpoint)
        .and_then(|targets| targets.get(target_id))
        .and_then(Weak::upgrade)
    {
        return slot;
    }
    registry.retain(|_, targets| {
        targets.retain(|_, slot| slot.strong_count() > 0);
        !targets.is_empty()
    });
    let slot = Arc::default();
    registry
        .entry(endpoint.into())
        .or_default()
        .insert(target_id.into(), Arc::downgrade(&slot));
    slot
}
