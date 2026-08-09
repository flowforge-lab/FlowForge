//! Agent autonomy mode. A named preset over two existing axes — tool capability
//! (`Safety`) and approval policy (the #229 gate) — surfaced as a single switch in
//! the composer and the `--mode` CLI flag. See RFC 0011.
//!
//! This type IS part of the IPC/settings surface, exported to TypeScript via `ts-rs`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Prefix of the transcript marker persisted when the user switches mode
/// mid-session (e.g. `[system: Mode switched to Auto...]`). The marker is stored
/// as a [`Role::User`](crate::Role) message so it stays in-position for every
/// provider (Bedrock/Anthropic would otherwise lift a `Role::System` message out
/// of sequence, losing the temporal signal — #848). OpenAI-compatible and Ollama
/// wire translators promote a `user` message carrying this prefix back to
/// `role: "system"` in-position, restoring the true system-notification semantics
/// for backends that serialize system messages where they sit (#850).
///
/// Single source of truth shared by the producer (the desktop `set_session_mode`
/// command) and the wire consumers, so the discriminator cannot drift. Matched
/// with `starts_with`, never `contains`: a user turn that merely mentions the
/// text mid-sentence must stay a user turn.
pub const MODE_SWITCH_MARKER_PREFIX: &str = "[system:";

/// How much autonomy the agent has before it touches the world. The per-tier
/// approval outcome is set by the permission matrix (RFC 0019 §3); the summaries
/// below describe its default cells.
///
/// - [`Mode::Plan`] advertises only `Safety::ReadOnly` tools, so the model cannot
///   even see — let alone call — anything that mutates. Safe by construction.
/// - [`Mode::Act`] advertises the full registry; ReadOnly, Write, and Sensitive
///   are auto-approved and Dangerous prompts for confirmation. Full access.
/// - [`Mode::Auto`] advertises the full registry; ReadOnly and Write are
///   auto-approved, Sensitive prompts, and Dangerous is denied. The factory default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum Mode {
    /// Read-and-think only: only ReadOnly tools are advertised; nothing can mutate.
    Plan,
    /// Full registry; ReadOnly/Write/Sensitive auto-approved, Dangerous prompts.
    Act,
    /// Full registry; ReadOnly/Write auto-approved, Sensitive prompts, Dangerous
    /// denied. Default.
    #[default]
    Auto,
}

impl Mode {
    /// Whether this mode restricts the advertised toolset to ReadOnly tools only.
    pub fn is_plan(self) -> bool {
        matches!(self, Mode::Plan)
    }
}
