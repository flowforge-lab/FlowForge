//! User-override layer for the model context-window lookup (#457).
//!
//! The schema, bundled defaults, and pure lookups live in [`ff_core::model_specs`]
//! (I/O-free, so capability lookups in other crates can share them). This module
//! adds the optional **runtime override**: an on-disk
//! `<config dir>/flowforge/model-specs.json` whose rules are *prepended* to the
//! bundled rules, so a user can both override (a more-specific match wins) and
//! extend (new families) the defaults without a code change.
//!
//! The merged rule set is read once and cached. A file that exists but cannot be
//! read or parsed is preserved (renamed to a `*.corrupt-<unix>.json` sibling) and
//! treated as absent, so a truncated or hand-mangled override never silently
//! changes budgets — it falls back to the bundled defaults. Mirrors the
//! provider-registry read in `apps/desktop/src-tauri/src/state.rs`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ff_core::{bundled_rules, context_window_in, parse_specs, ModelSpec, ModelSpecs};

/// Outcome of reading the user override file. Distinguishing `Corrupt` from
/// `Absent` is what keeps a damaged file from silently masquerading as "no
/// override" only after it has been destroyed; instead it is preserved.
enum SpecsRead {
    Loaded(ModelSpecs),
    Absent,
    Corrupt,
}

/// User override path: `<config dir>/flowforge/model-specs.json`, matching the
/// provider registry and phenotype files.
fn user_specs_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("model-specs.json"))
}

/// Read and parse the user override without ever destroying data on failure. A
/// file that exists but is unreadable/unparseable is renamed to a
/// `*.corrupt-<unix>.json` sibling and reported as [`SpecsRead::Corrupt`].
fn read_user_specs(path: Option<&Path>) -> SpecsRead {
    let Some(path) = path else {
        return SpecsRead::Absent;
    };
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return SpecsRead::Absent,
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e,
                "model-specs override unreadable; quarantining and using bundled defaults");
            quarantine(path);
            return SpecsRead::Corrupt;
        }
    };
    match parse_specs(&raw) {
        Ok(specs) => SpecsRead::Loaded(specs),
        Err(e) => {
            tracing::error!(path = %path.display(), error = %e,
                "model-specs override unparseable; quarantining and using bundled defaults");
            quarantine(path);
            SpecsRead::Corrupt
        }
    }
}

/// Preserve an unreadable override by renaming it alongside the original rather
/// than letting it be ignored-then-lost. Best-effort: a rename failure is logged
/// but never fatal.
fn quarantine(path: &Path) {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let preserved = path.with_extension(format!("corrupt-{unix}.json"));
    match std::fs::rename(path, &preserved) {
        Ok(()) => {
            tracing::warn!(preserved = %preserved.display(), "preserved unreadable model-specs override")
        }
        Err(e) => tracing::warn!(path = %path.display(), error = %e,
            "could not preserve unreadable model-specs override"),
    }
}

/// User rules prepended to the bundled rules (override **and** extend), so the
/// first match — scanning user rules first — wins.
fn merged_rules(user_path: Option<&Path>) -> Vec<ModelSpec> {
    let mut rules = match read_user_specs(user_path) {
        SpecsRead::Loaded(specs) => specs.rules,
        SpecsRead::Absent | SpecsRead::Corrupt => Vec::new(),
    };
    rules.extend(bundled_rules().iter().cloned());
    rules
}

/// Context window for a model id, read from the merged (user + bundled) rules.
/// The merged set is cached after the first call.
pub(crate) fn lookup(model: &str) -> u64 {
    static MERGED: OnceLock<Vec<ModelSpec>> = OnceLock::new();
    let rules = MERGED.get_or_init(|| merged_rules(user_specs_path().as_deref()));
    context_window_in(rules, model)
}

#[cfg(test)]
mod tests;
