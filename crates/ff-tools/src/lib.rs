//! Built-in tools the agent can call: `bash`, `view`, `edit`, `write`, `grep`,
//! `glob`, `tree`, `todo`, `web_fetch`, `ask_user`. The `web_search` tool lives
//! in the desktop crate (it reads user-configured search settings).
//!
//! File tools ([`view`], [`edit`], [`write`]) are hard-jailed to a per-session workspace root
//! via [`jail::resolve_in_root`]. `bash` runs in that root as its working directory
//! but is not sandboxed (see [`bash`]); safety leans on [`registry::Safety`]
//! classification plus a host-supplied approval gate.
//!
//! Search/discovery tools ([`grep`], [`glob`], [`tree`]) are read-only and jailed
//! to the same root; [`todo`] is a stateless planning checklist (full-replace).
//!
//! [`web_fetch`] reaches the network: it is `Safety::Write` (approval-gated) and
//! guarded against SSRF (internal/loopback/cloud-metadata targets) by
//! [`url_safety::SsrfPolicy`].
//!
//! [`ask_user`] is interactive: it pauses the turn for user input (#44) rather than
//! executing, so the agent loop routes it through the host's `Approver::ask`.

mod ask_user;
mod bash;
mod edit;
mod glob;
mod grep;
mod html_text;
mod jail;
pub mod memory;
mod registry;
mod todo;
mod tree;
pub mod url_safety;
mod view;
mod web_fetch;
mod write;

pub use registry::{Safety, Tool, ToolOutcome, ToolRegistry};
pub use url_safety::SsrfPolicy;
