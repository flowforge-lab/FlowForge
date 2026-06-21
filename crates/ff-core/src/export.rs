use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The serialization target for [`export_session`](../../ff_session). `Json` is a
/// lossless, round-trippable dump of the session and its transcript; `Markdown` is
/// a folded, human-readable transcript for sharing or archiving (RFC 0012, #278).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum Format {
    Markdown,
    Json,
}
