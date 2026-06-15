//! Domain types shared across FlowForge crates and exported to TypeScript via `ts-rs`.
//!
//! These types ARE the IPC contract. Changing one is a breaking change for the frontend —
//! regenerate bindings (`cargo test`) and update the mock in the same PR.

pub mod events;
mod message;
mod provider;
mod session;
mod skill;

pub use message::{Message, Role, ToolCall};
pub use provider::{ProviderConfig, ProviderKind};
pub use session::{auto_title, Session, SessionStatus};
pub use skill::{Phenotype, Skill, SkillInfo, SkillManifest};
