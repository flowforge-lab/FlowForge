//! Built-in tools the agent can call: `bash`, `python`, `view`, `edit`, `write`,
//! `apply_patch`, `grep`, `glob`, `tree`, `todo`, `web_fetch`, `web_search`, `ask_user`.
//!
//! File tools ([`view`], [`edit`], [`write`]) are hard-jailed to a per-session workspace root
//! via [`jail::resolve_in_root`]. `bash` runs in that root as its working directory
//! but is not sandboxed (see [`bash`]); safety leans on [`registry::Safety`]
//! classification plus a host-supplied approval gate.
//!
//! Search/discovery tools ([`grep`], [`glob`], [`tree`]) are read-only and jailed
//! to the same root; [`todo`] is a stateless planning checklist (full-replace).
//!
//! [`web_fetch`] and [`web_search`] reach the network: both are `Safety::Write`
//! (approval-gated) and guarded against SSRF (internal/loopback/cloud-metadata
//! targets) by [`url_safety::SsrfPolicy`]. [`web_search`] is stateful -- it reads a
//! host-injected [`web_search::WebSearchTool`] config ([`ff_core::SearchConfig`]) at
//! call time -- so the host constructs it via [`web_search::WebSearchTool::new`].
//!
//! [`ask_user`] is interactive: it pauses the turn for user input (#44) rather than
//! executing, so the agent loop routes it through the host's `Approver::ask`.

mod agent_tool;
mod apply_patch;
mod ask_user;
mod bash;
mod compaction;
mod diagnostics;
mod edit;
mod git;
mod github;
mod glob;
mod goal_complete;
mod grep;
mod html_text;
mod jail;
pub mod memory;
pub mod notebook;
pub mod process;
mod python;
mod registry;
mod shell;
mod sink;
mod test_runner;
mod todo;
mod tree;
pub mod url_safety;
mod view;
mod web_fetch;
pub mod web_search;
mod write;

pub use agent_tool::{AgentTool, AGENT_TOOL_NAME};
pub use compaction::{CompactionRetrieveTool, COMPACTION_RETRIEVE_TOOL};
pub use goal_complete::{GoalCompleteTool, GOAL_COMPLETE_TOOL_NAME};
// The workspace path-jail entry point, surfaced to the desktop crate's file
// commands (#872). Only `resolve_in_root` is re-exported — the module itself
// stays private so its internals aren't part of the crate's public API.
pub use jail::resolve_in_root;
pub use notebook::{KernelLiveState, KernelSupervisor, NotebookKernelState, NotebookTool};
pub use registry::{is_subagent, Safety, Tool, ToolOutcome, ToolRegistry};
pub use sink::{OutputSink, OutputStream};
pub use url_safety::SsrfPolicy;
pub use web_search::{SearchKeyProvider, WebSearchTool};
