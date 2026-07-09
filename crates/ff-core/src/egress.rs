//! Network-egress policy for a phenotype (RFC 0013). A coarse boundary over the
//! *tool* layer — orthogonal to [`crate::Mode`], which gates tool *capability*
//! (Safety). Pinning a local model only stops *inference* egress; a network-capable
//! tool (`web_fetch`, a `bash` curl, an outbound MCP server) can still ship PII out.
//! `LocalOnly` strips network-capable tools from the *advertised* set, reusing the
//! same toolset-filtering seam as Plan mode (RFC 0011 / #240).
//!
//! This type IS part of the IPC/settings surface, exported to TypeScript via `ts-rs`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Whether a phenotype may use network-capable tools.
///
/// - [`Egress::Open`] — all tools advertised (today's behaviour). The default, so
///   existing phenotypes and on-disk TOML that omit `egress` are unchanged.
/// - [`Egress::LocalOnly`] — network-capable tools are stripped from the advertised
///   set, the same way Plan mode strips non-ReadOnly tools. Fail-safe: a tool is
///   treated as network-capable unless it proves otherwise (see
///   [`crate::permission`]-adjacent `Tool::reaches_network` in `ff-tools`).
///
/// Serializes as `camelCase` (`open` / `localOnly`) for a consistent TS binding
/// with [`crate::Mode`]; a `local-only` alias is accepted on read so the RFC 0013
/// TOML literal (`egress = "local-only"`) still deserializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum Egress {
    /// All tools advertised. The backward-compatible default.
    #[default]
    Open,
    /// Network-capable tools are stripped from the advertised set.
    #[serde(alias = "local-only")]
    LocalOnly,
}

impl Egress {
    /// Whether this policy restricts the advertised toolset to local-only tools.
    pub fn is_local_only(self) -> bool {
        matches!(self, Egress::LocalOnly)
    }
}
