//! Skill discovery, manifest parsing, and filesystem hot-reload (M3).
//!
//! Loads skills from `~/.flowforge/skills/<name>/SKILL.md` into a [`SkillRegistry`]
//! and keeps it current via a [`SkillWatcher`]. Depends only on `ff-core` — tool
//! resolution against the `ToolRegistry` happens at the agent/installer boundary,
//! not here.

mod error;
mod install;
mod parse;
mod phenotype;
mod registry;
mod search;
mod version;
mod watch;

pub use error::SkillError;
pub use install::{
    commit_install, install, prepare_install, uninstall, InstallError, StagedInstall,
};
pub use parse::parse_skill;
pub use phenotype::{default_phenotype, load_phenotypes, PhenotypeError, DEFAULT_PHENOTYPE};
pub use registry::{first_executable, first_executable_recursive, SkillRegistry};
pub use search::{search_skills, SkillHit};
pub use version::{bump_patch, bump_skill, list_skill_versions, rollback_skill, VersionError};
pub use watch::{SharedRegistry, SkillWatcher};
