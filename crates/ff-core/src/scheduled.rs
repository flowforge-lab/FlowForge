//! Scheduled-task wire types (RFC 0017). These ARE the IPC contract for
//! Settings → Scheduled; the durable store lives in `ff-scheduled` and the cron
//! runner in `ff-scheduled`/desktop (#542). Changing a type here is a breaking
//! change for the frontend — regenerate bindings and update the mock.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The most a headless scheduled fire may do without a human present (RFC 0017
/// §3). Enforced by the runner's `ScheduledApprover` (#542); `Dangerous` tool
/// calls are always denied headless regardless of this ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum SafetyCeiling {
    /// Read-only tools only — the safe default.
    #[default]
    ReadOnly,
    /// Read + write tools (e.g. file edits) auto-approve; still no `Dangerous`.
    Write,
}

/// A named internal action a built-in task runs, as opposed to a free-text
/// prompt. Dispatch + seeding is #544; the type lives here so the FE `Builtin`
/// badge has a contract from the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum BuiltinAction {
    /// Run a memory consolidation pass ("Memory Organizer" in the UI).
    MemoryConsolidate,
}

/// What a fire does. A sum type so the illegal state "a built-in with a
/// free-text prompt" is unrepresentable (RFC 0017 §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum TaskKind {
    /// A user task: free-text instructions handed to a headless agent run.
    Prompt(String),
    /// An app task: a named internal action; cannot be deleted by the user.
    Builtin(BuiltinAction),
}

impl TaskKind {
    /// Whether this task is a built-in (drives the FE badge + delete guard).
    pub fn is_builtin(&self) -> bool {
        matches!(self, TaskKind::Builtin(_))
    }
}

/// A cron-scheduled agent task as presented in Settings → Scheduled (RFC 0017).
/// `cadenceLabel` and `nextRun` are derived from `cron` on read and never
/// persisted, so they cannot drift from the stored expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    /// The raw cron expression (6-field `sec min hour dom mon dow`, the form the
    /// `cron` crate parses). Derived labels come from this.
    pub cron: String,
    pub kind: TaskKind,
    /// Working directory the fire's session runs in; `None` inherits the global
    /// default workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace: Option<String>,
    /// Phenotype/profile the fire runs as; `None` inherits the global active one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub profile: Option<String>,
    pub safety_ceiling: SafetyCeiling,
    pub paused: bool,
    /// Human cadence summary derived from `cron` (e.g. "Daily at 5:00 PM").
    /// Server-computed on read; never stored.
    pub cadence_label: String,
    /// Next fire time as epoch ms, computed from `cron` on read. `None` when the
    /// expression has no future occurrence. Display-only — NOT the fire trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub next_run: Option<i64>,
    /// Last fire time as epoch ms; `None` until the task has fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub last_run: Option<i64>,
}

/// Payload for `create_scheduled_task`. Carries the fields the New-task form
/// collects; `id`/derived fields are server-assigned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct CreateScheduledTaskInput {
    pub name: String,
    pub cron: String,
    pub kind: TaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub profile: Option<String>,
    #[serde(default)]
    pub safety_ceiling: SafetyCeiling,
}

/// Terminal status of one fire (RFC 0017 §8.4). A tool denial is incidental
/// within an otherwise-`Ok` run, not a terminal status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum RunStatus {
    Ok,
    Error,
    Cancelled,
    /// The turn fired but called `ask_user` and was dismissed (no interactive
    /// surface in a headless fire), so it surfaced for the user rather than
    /// continuing on a dismissed answer (RFC 0017 §3.1, §8.4).
    NeedsAttention,
}

/// A record of one fire, backing the ↗ open-session affordance and the audit
/// trail (#544 surfaces it; the type + store land here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct RunRecord {
    #[ts(type = "number")]
    pub id: i64,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
    #[ts(type = "number")]
    pub fired_ms: i64,
    pub status: RunStatus,
}
