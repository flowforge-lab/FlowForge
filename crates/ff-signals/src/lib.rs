//! Intention/outcome signal bus. FlowForge emits these; NeuroForge consumes them.
//!
//! M1 defines the signal vocabulary (re-exported from `ff-core::events`). The actual
//! RPE/flow computation lives in the separate NeuroForge plugin system.

pub use ff_core::events::{IntentionSignal, OutcomeSignal};

/// A signal emitted by FlowForge for downstream cognitive-health analysis.
#[derive(Debug, Clone)]
pub enum Signal {
    Intention(IntentionSignal),
    Outcome(OutcomeSignal),
}
