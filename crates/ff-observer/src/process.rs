//! Process source stub (#893, Phase 3). The supervisor rejects `start`
//! calls for this kind with an actionable error before it ever
//! instantiates a source, so this module is unreachable in Phase 1. Kept
//! so the kind variant compiles and the future Phase 3 PR is a clean
//! fill-in.

use super::source::{ObserverContext, ObserverEvent, ObserverSource};
use std::sync::Arc;
use tokio::sync::Notify;

pub struct ProcessSource;

#[async_trait::async_trait]
impl ObserverSource for ProcessSource {
    fn ctx(&self) -> &ObserverContext {
        unreachable!(
            "ProcessSource is not implemented in Phase 1; supervisor.start should have rejected it"
        )
    }

    async fn next_event(&mut self, _cancel: Arc<Notify>) -> Option<ObserverEvent> {
        unreachable!("ProcessSource is not implemented in Phase 1")
    }
}
