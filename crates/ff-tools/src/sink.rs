//! Live output streaming for long-running tools (#680).
//!
//! Some tools (notably [`crate::bash`]) buffer all stdout/stderr until the process
//! exits, so a slow build or test shows nothing until completion. An [`OutputSink`]
//! lets such a tool push output chunks *as they are produced*, in addition to the
//! full capture it still returns in its final [`crate::ToolOutcome`]. The live
//! stream is purely additive: the stored/returned result is unchanged, and the
//! agent loop forwards each chunk to the frontend so the running tool-call block
//! updates in place.
//!
//! The sink is a cheap `Clone + Send` handle over an unbounded channel. Dropping
//! every clone (e.g. when the tool finishes) closes the channel, which the drain
//! side observes as end-of-stream.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

/// Which standard stream a chunk came from. Kept distinct so the frontend can
/// render stderr differently from stdout if it chooses; the backend interleaves
/// them in arrival order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// A handle a tool uses to emit live output chunks. Additive to the tool's final
/// capture — see the module docs. Cloneable and cheap; send failures (no receiver)
/// are ignored, so a tool never blocks or errors on a dropped consumer.
#[derive(Debug, Clone)]
pub struct OutputSink {
    tx: UnboundedSender<(OutputStream, String)>,
}

impl OutputSink {
    pub fn new(tx: UnboundedSender<(OutputStream, String)>) -> Self {
        Self { tx }
    }

    /// Emit a chunk. Best-effort: if the receiver is gone the chunk is dropped
    /// silently, so streaming can never stall or fail the underlying tool.
    pub fn emit(&self, stream: OutputStream, delta: String) {
        let _ = self.tx.send((stream, delta));
    }
}
