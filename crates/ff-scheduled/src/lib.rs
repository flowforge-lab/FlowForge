//! `ff-scheduled` — durable store and cron derivation for FlowForge's
//! scheduled tasks (RFC 0017, #539). The wire types live in `ff-core`; this
//! crate persists tasks (SQLite, mirroring `ff-session`) and owns the cron
//! parsing / `next_run` / `prev_occurrence` / `cadence_label` logic. The
//! headless runner that fires due tasks is a separate concern (#542).

pub mod cron;
pub mod store;

pub use store::{DeleteBuiltinError, ScheduledStore};

// Re-export the wire types so callers can `use ff_scheduled::ScheduledTask`.
pub use ff_core::{
    BuiltinAction, CreateScheduledTaskInput, RunRecord, RunStatus, SafetyCeiling, ScheduledTask,
    TaskKind,
};
