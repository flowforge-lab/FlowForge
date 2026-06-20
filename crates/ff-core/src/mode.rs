//! Agent autonomy mode. A named preset over two existing axes — tool capability
//! (`Safety`) and approval policy (the #229 gate) — surfaced as a single switch in
//! the composer and the `--mode` CLI flag. See RFC 0011.
//!
//! This type IS part of the IPC/settings surface, exported to TypeScript via `ts-rs`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How much autonomy the agent has before it touches the world.
///
/// - [`Mode::Plan`] advertises only `Safety::ReadOnly` tools, so the model cannot
///   even see — let alone call — anything that mutates. Safe by construction.
/// - [`Mode::Act`] advertises the full registry; Write calls go through the approval
///   gate and Dangerous calls always prompt. This is FlowForge's historical behaviour.
/// - [`Mode::Auto`] advertises the full registry and auto-approves Write, but
///   Dangerous calls still always prompt. The factory default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum Mode {
    /// Read-and-think only: only ReadOnly tools are advertised; nothing can mutate.
    Plan,
    /// Full registry; Write is approval-gated, Dangerous always prompts.
    Act,
    /// Full registry; Write is auto-approved, Dangerous always prompts. Default.
    #[default]
    Auto,
}

impl Mode {
    /// Whether this mode restricts the advertised toolset to ReadOnly tools only.
    pub fn is_plan(self) -> bool {
        matches!(self, Mode::Plan)
    }
}
