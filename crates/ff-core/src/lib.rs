//! Domain types shared across FlowForge crates and exported to TypeScript via `ts-rs`.
//!
//! These types ARE the IPC contract. Changing one is a breaking change for the frontend —
//! regenerate bindings (`cargo test`) and update the mock in the same PR.

mod egress;
pub mod events;
mod export;
mod files;
mod goal;
mod mcp;
mod memory;
mod message;
mod mode;
mod model_specs;
pub mod permission;
mod provider;
mod scheduled;
mod search;
mod session;
mod skill;
mod spawn_env;

pub use egress::Egress;
pub use export::Format;
pub use files::{DirEntry, FileContent};
pub use goal::{
    goal_store_dir, Goal, GoalBudget, GoalLedgerEntry, GoalSpend, GoalStatus, GoalStore,
    NextAction, StepStatus, Verdict, DEFAULT_MAX_ITERATIONS,
};
pub use mcp::{McpScope, McpServerConfig, McpServerState, McpServerStatus, McpToolInfo};
pub use memory::{MemoryChunkStat, MemoryFileInfo, MemoryFileKind, MemoryOverview, Stratum};
pub use message::{
    Attachment, AttachmentKind, AttachmentSource, Message, Role, StopReason, ToolCall,
};
pub use mode::Mode;
pub use model_specs::{
    bundled_rules, context_window_in, parse_specs, supports_vision_in, ModelSpec, ModelSpecs,
    DEFAULT_CONTEXT_WINDOW_TOKENS,
};
pub use permission::{
    pre_prompt_decision, PermissionCell, PermissionMatrix, PermissionMatrixEntry,
    PermissionMatrixView, PermissionOverrideEntry, PermissionRule, PrePromptDecision, RuleEffect,
    Safety,
};
pub use provider::{
    model_supports_documents, model_supports_vision, BedrockAuth, ConnectionId,
    ContextWindowSource, ModelSelection, ProviderConfig, ProviderConnection, ProviderKind,
    ProviderRegistry, ReasoningEffort, ReasoningVisibility, ResolvedModel, SecretKind,
};
pub use scheduled::{
    BuiltinAction, CreateScheduledTaskInput, RunRecord, RunStatus, SafetyCeiling, ScheduledTask,
    TaskKind,
};
pub use search::{SearchBackend, SearchConfig, SearchSecretPresence};
pub use session::{auto_title, Session, SessionStatus, SessionWorkspace};
pub use skill::{Phenotype, Skill, SkillInfo, SkillManifest};
pub use spawn_env::{augment_path, augmented_path, extra_path_dirs};
