//! Instance keying for scoped MCP servers (RFC 0018 section 4.2).
//!
//! Before RFC 0018 the supervisor keyed its handle map by `server_id` alone: exactly
//! one instance per configured server. That is correct for a `Global`-scope server
//! (one shared process for the whole app) but wrong for a `Workspace`-scope server
//! (e.g. codegraph), which must run one process per distinct workspace root so two
//! sessions open on different checkouts each see their own code graph (#557).
//!
//! [`InstanceKey`] re-keys the map on the composite `(id, ScopeKey)`:
//!
//! - A `Global` server -> `InstanceKey { id, Global }`. Exactly one, as before.
//! - A `Workspace` server resolved for a session on `/path` ->
//!   `InstanceKey { id, Workspace(canonical("/path")) }`. Two sessions on the same
//!   canonical path share one instance (ref-counted); two on different paths get two.

use std::path::{Path, PathBuf};

/// The scope half of an [`InstanceKey`]. `Ord` so an `InstanceKey` can key a
/// `BTreeMap` with deterministic iteration.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScopeKey {
    /// One shared instance for the whole app (today's behavior).
    Global,
    /// One instance per distinct, canonicalized workspace root.
    Workspace(PathBuf),
}

impl ScopeKey {
    /// Build a `Workspace` key, canonicalizing `root` so two spellings of the same
    /// directory (symlinks, `.`/`..`, trailing slash) collapse to one instance.
    /// Falls back to the path as-given when canonicalization fails (a not-yet-created
    /// dir), so keying never silently drops a workspace.
    pub fn workspace(root: &Path) -> Self {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        ScopeKey::Workspace(canonical)
    }

    /// The workspace root this key carries, or `None` for `Global`. Used at connect
    /// to advertise the root (and as a belt-and-braces cwd) for a workspace server.
    pub fn root(&self) -> Option<&Path> {
        match self {
            ScopeKey::Global => None,
            ScopeKey::Workspace(p) => Some(p),
        }
    }

    /// A short human-readable label for the status snapshot (`None` for `Global`,
    /// the path string for a workspace instance) so the UI can disambiguate two
    /// instances of the same server id.
    pub fn display(&self) -> Option<String> {
        match self {
            ScopeKey::Global => None,
            ScopeKey::Workspace(p) => Some(p.display().to_string()),
        }
    }
}

/// The composite key the supervisor's handle map is keyed by (RFC 0018 section 4.2).
/// `Ord` (via `id` then `scope`) for deterministic `BTreeMap` iteration.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstanceKey {
    pub id: String,
    pub scope: ScopeKey,
}

impl InstanceKey {
    /// The single global instance for server `id`.
    pub fn global(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            scope: ScopeKey::Global,
        }
    }

    /// The workspace instance for server `id` rooted at the canonicalized `root`.
    pub fn workspace(id: impl Into<String>, root: &Path) -> Self {
        Self {
            id: id.into(),
            scope: ScopeKey::workspace(root),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_keys_with_same_path_are_equal() {
        let tmp = std::env::temp_dir();
        let a = InstanceKey::workspace("codegraph", &tmp);
        let b = InstanceKey::workspace("codegraph", &tmp);
        assert_eq!(a, b);
    }

    #[test]
    fn workspace_keys_with_different_paths_differ() {
        let a = InstanceKey::workspace("codegraph", Path::new("/tmp/aaa-ff-test"));
        let b = InstanceKey::workspace("codegraph", Path::new("/tmp/bbb-ff-test"));
        assert_ne!(a, b);
    }

    #[test]
    fn global_and_workspace_keys_differ() {
        let g = InstanceKey::global("codegraph");
        let w = InstanceKey::workspace("codegraph", Path::new("/tmp/ff-test"));
        assert_ne!(g, w);
        assert_eq!(g.scope.display(), None);
        assert_eq!(w.scope.display(), Some("/tmp/ff-test".to_string()));
    }

    #[test]
    fn canonicalization_collapses_dot_segments() {
        let tmp = std::env::temp_dir();
        let dotted = tmp.join(".");
        let a = ScopeKey::workspace(&tmp);
        let b = ScopeKey::workspace(&dotted);
        assert_eq!(a, b);
    }
}
