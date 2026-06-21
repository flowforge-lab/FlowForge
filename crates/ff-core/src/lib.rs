//! Domain types shared across FlowForge crates and exported to TypeScript via `ts-rs`.
//!
//! These types ARE the IPC contract. Changing one is a breaking change for the frontend —
//! regenerate bindings (`cargo test`) and update the mock in the same PR.

pub mod events;
mod export;
mod mcp;
mod memory;
mod message;
mod mode;
mod provider;
mod search;
mod session;
mod skill;

pub use export::Format;
pub use mcp::{McpServerConfig, McpServerState, McpServerStatus, McpToolInfo};
pub use memory::{MemoryFileInfo, MemoryFileKind, MemoryOverview};
pub use message::{Message, Role, ToolCall};
pub use mode::Mode;
pub use provider::{
    BedrockAuth, ConnectionId, ProviderConfig, ProviderConnection, ProviderKind, ProviderRegistry,
    SecretKind,
};
pub use search::{SearchBackend, SearchConfig};
pub use session::{auto_title, Session, SessionStatus, SessionWorkspace};
pub use skill::{Phenotype, Skill, SkillInfo, SkillManifest};
