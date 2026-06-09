use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum SessionStatus {
    Active,
    Done,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct Session {
    pub id: String,
    /// The stated intention for this session (Intention-Aware Sessions principle).
    pub goal: Option<String>,
    pub status: SessionStatus,
    /// Unix epoch milliseconds.
    #[ts(type = "number")]
    pub created_at: i64,
    /// Unix epoch milliseconds.
    #[ts(type = "number")]
    pub updated_at: i64,
}
