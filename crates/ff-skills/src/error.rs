use std::path::PathBuf;

/// Why a single skill failed to load. `load_dir` collects these per-skill rather
/// than failing the whole registry — one broken skill must not blank the rest.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: missing frontmatter (expected a leading `---` block)")]
    MissingFrontmatter { path: PathBuf },
    #[error("{path}: invalid frontmatter: {source}")]
    Frontmatter {
        path: PathBuf,
        #[source]
        source: serde_norway::Error,
    },
    #[error("{path}: contains an executable file ({file}) — skills are instructions only")]
    ExecutablePresent { path: PathBuf, file: String },
    #[error("duplicate skill name `{name}` at {path} (already loaded)")]
    DuplicateName { name: String, path: PathBuf },
}
