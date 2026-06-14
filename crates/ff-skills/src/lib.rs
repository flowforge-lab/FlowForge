//! Skill discovery, manifest parsing, and filesystem hot-reload (M3).
//!
//! Loads skills from `~/.flowforge/skills/<name>/SKILL.md` into a [`SkillRegistry`]
//! and keeps it current via a [`SkillWatcher`]. Depends only on `ff-core` — tool
//! resolution against the `ToolRegistry` happens at the agent/installer boundary,
//! not here.

mod error;
mod install;
mod parse;
mod registry;
mod watch;

pub use error::SkillError;
pub use install::{
    commit_install, install, prepare_install, uninstall, InstallError, StagedInstall,
};
pub use parse::parse_skill;
pub use registry::{first_executable, first_executable_recursive, SkillRegistry};
pub use watch::{SharedRegistry, SkillWatcher};
