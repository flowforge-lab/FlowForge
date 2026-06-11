//! Domain types shared across FlowForge crates and exported to TypeScript via `ts-rs`.
//!
//! These types ARE the IPC contract. Changing one is a breaking change for the frontend —
//! regenerate bindings (`cargo test`) and update the mock in the same PR.

pub mod events;
mod message;
mod session;

pub use message::{Message, Role, ToolCall};
pub use session::{Session, SessionStatus};
