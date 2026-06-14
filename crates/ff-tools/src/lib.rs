//! Built-in tools the agent can call: `bash`, `view`, `edit`, `write`, `grep`,
//! `glob`, `tree`, `todo`.
//!
//! File tools ([`view`], [`edit`], [`write`]) are hard-jailed to a per-session workspace root
//! via [`jail::resolve_in_root`]. `bash` runs in that root as its working directory
//! but is not sandboxed (see [`bash`]); safety leans on [`registry::Safety`]
//! classification plus a host-supplied approval gate.
//!
//! Search/discovery tools ([`grep`], [`glob`], [`tree`]) are read-only and jailed
//! to the same root; [`todo`] is a stateless planning checklist (full-replace).

mod bash;
mod edit;
mod glob;
mod grep;
mod jail;
mod registry;
mod todo;
mod tree;
mod view;
mod write;

pub use registry::{Safety, Tool, ToolOutcome, ToolRegistry};
