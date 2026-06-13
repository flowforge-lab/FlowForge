//! Built-in tools the agent can call: `bash`, `view`, `edit`, `write`.
//!
//! File tools ([`view`], [`edit`], [`write`]) are hard-jailed to a per-session workspace root
//! via [`jail::resolve_in_root`]. `bash` runs in that root as its working directory
//! but is not sandboxed (see [`bash`]); safety leans on [`registry::Safety`]
//! classification plus a host-supplied approval gate.

mod bash;
mod edit;
mod jail;
mod registry;
mod view;
mod write;

pub use registry::{Safety, Tool, ToolOutcome, ToolRegistry};
