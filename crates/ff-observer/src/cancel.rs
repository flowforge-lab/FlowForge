//! A tiny newtype around `tokio::sync::Notify` so source modules don't have
//! to plumb `Arc<Notify>` through every signature. Equivalent in
//! expressiveness, slightly nicer in the trace logs (the `Debug` impl
//! prints `"Cancel"` instead of `"Notify"`).

use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<Notify>);

impl Cancel {
    pub fn new() -> Self {
        Self(Arc::new(Notify::new()))
    }

    /// Ask the source to stop. Implemented with
    /// [`Notify::notify_one`] (not `notify_waiters`) because the
    /// source task may not be scheduled yet — `start` returned before
    /// the runtime had a chance to poll its first `select!` arm, so
    /// `notify_waiters` would silently miss every waiter. `notify_one`
    /// stores a permit that the next `notified().await` consumes,
    /// which is what we need: the cancel must win the race even when
    /// the supervisor races the scheduler.
    pub fn signal(&self) {
        self.0.notify_one();
    }

    pub fn as_notify(&self) -> Arc<Notify> {
        self.0.clone()
    }
}
