//! Persisted provider-registry IO for the `flowforge config` subcommand (#724).
//!
//! Reads and writes `<config_dir>/flowforge/provider-registry.json` — the same
//! file the desktop's settings panel writes. Atomic rename keeps a crash
//! mid-write from leaving a truncated file behind; an unreadable file is
//! quarantined (renamed to `*.corrupt-<unix>.json`) so a future save cannot
//! silently destroy the user's bytes. A slim port of the corresponding helpers
//! in `apps/desktop/src-tauri/src/state.rs` — same shape, same quarantine
//! convention, same atomic-write primitive.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ff_core::{ProviderConfig, ProviderConnection, ProviderRegistry};

/// `<config dir>/flowforge/provider-registry.json` — the persisted connection
/// registry. `None` only when the OS exposes no config dir, in which case
/// settings stay in-memory for the session (mirrors the desktop).
pub fn registry_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("flowforge").join("provider-registry.json"))
}

/// Legacy `<config dir>/flowforge/provider.json` — read once, on first registry
/// miss, to migrate a pre-registry desktop install. Same path the desktop
/// checks against; kept in sync by convention.
fn legacy_config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("flowforge").join("provider.json"))
}

/// Resolved config dir: the test override (a thread-local set by
/// `test_support::set_config_dir`) when active, else the OS default. The
/// thread-local lets integration tests point the runner at a tempdir without
/// touching the user's real config dir.
fn config_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(override_dir) = crate::test_support::config_dir_override() {
            return Some(override_dir);
        }
    }
    dirs::config_dir()
}

/// Build the registry to start from: the saved registry if present, else a
/// one-time migration of a legacy `provider.json` (the saved provider becomes
/// the *active* connection, with the other local vendor seeded keyless +
/// inactive), else the built-in default. Pure and idempotent — persistence
/// happens lazily on the first mutation, so construction (including in tests)
/// never writes to the config dir.
pub fn load_registry() -> ProviderRegistry {
    load_registry_at(registry_path(), legacy_config_path())
}

/// Outcome of reading the persisted registry file: cleanly loaded, genuinely
/// absent, or present-but-unreadable. Distinguishing the last case is what
/// keeps a corrupt or half-written file from silently masquerading as "no
/// config" and wiping the user's connections back to the factory default.
enum RegistryRead {
    Loaded(ProviderRegistry),
    Absent,
    Corrupt,
}

/// Read and parse the registry file without ever destroying data on failure.
/// A file that exists but cannot be read or parsed (e.g. truncated by a crash
/// mid-write) is renamed to a `*.corrupt-<unix>.json` sibling and reported as
/// [`RegistryRead::Corrupt`] so the caller seeds a fresh default without
/// overwriting the preserved bytes.
fn read_registry_file(path: Option<&Path>) -> RegistryRead {
    let Some(path) = path else {
        return RegistryRead::Absent;
    };
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return RegistryRead::Absent,
        Err(e) => {
            eprintln!(
                "warning: provider registry unreadable at {}: {e}; quarantining and seeding default",
                path.display()
            );
            quarantine_registry(path);
            return RegistryRead::Corrupt;
        }
    };
    match serde_json::from_str::<ProviderRegistry>(&raw) {
        Ok(registry) => RegistryRead::Loaded(registry),
        Err(e) => {
            eprintln!(
                "warning: provider registry unparseable at {}: {e}; quarantining and seeding default",
                path.display()
            );
            quarantine_registry(path);
            RegistryRead::Corrupt
        }
    }
}

/// Preserve an unreadable registry file by renaming it alongside the original
/// rather than letting the next save truncate it. Best-effort: a rename failure
/// is logged but never fatal.
fn quarantine_registry(path: &Path) {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let preserved = path.with_extension(format!("corrupt-{unix}.json"));
    match fs::rename(path, &preserved) {
        Ok(()) => eprintln!(
            "warning: preserved unreadable provider registry at {}",
            preserved.display()
        ),
        Err(e) => eprintln!(
            "warning: could not preserve unreadable provider registry at {}: {e}",
            path.display()
        ),
    }
}

/// Path-injectable core of [`load_registry`] so tests can drive it with tempdir
/// paths instead of the real config dir.
pub(crate) fn load_registry_at(
    reg_path: Option<PathBuf>,
    cfg_path: Option<PathBuf>,
) -> ProviderRegistry {
    let mut registry = match read_registry_file(reg_path.as_deref()) {
        RegistryRead::Loaded(registry) => registry,
        // Only a genuinely absent registry falls through to legacy migration; a
        // quarantined (corrupt) one seeds a clean default so stale legacy state
        // is never re-migrated over a registry the user was actively using.
        RegistryRead::Absent => cfg_path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<ProviderConfig>(&s).ok())
            .map(build_migrated_registry)
            .unwrap_or_default(),
        RegistryRead::Corrupt => ProviderRegistry::default(),
    };
    // Run any pending one-time migrations in memory (e.g. local-thinking flip);
    // the bumped schema_version persists on the next mutation via the save
    // call, so construction stays write-free.
    registry.migrate();
    registry
}

/// Migrate a legacy single [`ProviderConfig`] into a registry: it becomes the
/// active connection, and the *other* built-in local vendor is added keyless
/// and inactive so the user can still switch. Mirrors the desktop's
/// `build_migrated_registry` so a CLI user who also runs the desktop sees the
/// same connections.
fn build_migrated_registry(config: ProviderConfig) -> ProviderRegistry {
    let active = config_to_connection(config);
    let mut connections = vec![active.clone()];
    for seed in ProviderRegistry::default().connections {
        if seed.kind != active.kind {
            connections.push(seed);
        }
    }
    ProviderRegistry {
        active: active.id,
        connections,
        // Legacy `provider.json` predates schema_version; 0 lets the load-path
        // migration run once and stamp the current version on the next save.
        schema_version: 0,
    }
}

fn config_to_connection(config: ProviderConfig) -> ProviderConnection {
    let id = config.kind.slug().to_string();
    ProviderConnection {
        id: id.clone(),
        kind: config.kind,
        display_name: id,
        vendor: None,
        base_url: config.base_url,
        model: config.model,
        has_key: config.has_key,
        secret_missing: false,
        thinking: config.thinking,
        reasoning_effort: config.reasoning_effort,
        reasoning_visibility: config.reasoning_visibility,
        warmup_enabled: config.warmup_enabled,
        num_ctx: config.num_ctx,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    }
}

/// Atomically write `contents` to `path`: write a sibling `.tmp` file, then
/// rename it over the target. Rename is atomic on the same filesystem, so a
/// crash or kill mid-write leaves the previous (valid) file intact instead of
/// a truncated one.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

/// Persist the connection registry. Best-effort: a write failure leaves the
/// in-memory registry authoritative for this session rather than failing the
/// command, mirroring the desktop's `save_registry`.
pub fn save_registry(registry: &ProviderRegistry) -> Result<(), String> {
    let Some(path) = registry_path() else {
        return Err("no config directory; cannot persist registry".to_string());
    };
    let json =
        serde_json::to_string_pretty(registry).map_err(|e| format!("serialize registry: {e}"))?;
    write_atomic(&path, &json).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff_core::{ProviderConfig, ProviderKind};
    use std::fs;

    #[test]
    fn load_falls_back_to_default_when_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = load_registry_at(
            Some(tmp.path().join("provider-registry.json")),
            Some(tmp.path().join("provider.json")),
        );
        assert_eq!(loaded, ProviderRegistry::default());
    }

    #[test]
    fn corrupt_registry_is_quarantined_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let reg_path = tmp.path().join("provider-registry.json");
        // A truncated / half-written file (the crash-mid-write failure mode).
        fs::write(&reg_path, "{ \"connections\": [ {").unwrap();

        let loaded = load_registry_at(Some(reg_path.clone()), None);

        // A corrupt file seeds a clean default rather than silently nuking config.
        assert_eq!(loaded, ProviderRegistry::default());
        // The unreadable bytes are preserved in a sibling, never destroyed...
        let preserved: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("provider-registry.corrupt-")
            })
            .collect();
        assert_eq!(
            preserved.len(),
            1,
            "corrupt file should be quarantined once"
        );
        // ...and the original path is moved aside (so the next save starts clean).
        assert!(!reg_path.exists());
    }

    #[test]
    fn write_atomic_replaces_existing_and_leaves_no_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("provider-registry.json");
        write_atomic(&path, "first").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        // A second write fully replaces the contents (no append / partial state)...
        write_atomic(&path, "second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        // ...and never leaves the intermediate .tmp file behind.
        assert!(!tmp.path().join("provider-registry.tmp").exists());
    }

    #[test]
    fn save_registry_creates_parent_and_writes_valid_json() {
        let tmp = tempfile::tempdir().unwrap();
        // The tempdir IS the config dir; registry lives at <tmp>/flowforge/...
        // For this test we point save_registry_at (the test-only seam below) at
        // a path inside the tempdir so we don't touch the real config dir.
        let path = tmp.path().join("provider-registry.json");
        save_registry_at(&path, &ProviderRegistry::default()).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let back: ProviderRegistry = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, ProviderRegistry::default());
    }

    #[test]
    fn legacy_provider_json_is_migrated_into_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("provider.json");
        fs::write(
            &cfg_path,
            serde_json::to_string(&ProviderConfig {
                kind: ProviderKind::Ollama,
                model: "llama3".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        let loaded = load_registry_at(None, Some(cfg_path));
        assert_eq!(loaded.active, "ollama");
        // Migrated active plus the seeded non-ollama local backend (candle-vllm).
        assert_eq!(loaded.connections.len(), 2);
        // The active connection inherits the legacy model.
        let active = loaded
            .connections
            .iter()
            .find(|c| c.id == "ollama")
            .unwrap();
        assert_eq!(active.model, "llama3");
    }

    /// Test-only save seam that takes a path directly, so the real
    /// `save_registry` (which reads `dirs::config_dir()`) is never exercised
    /// by tests.
    fn save_registry_at(path: &Path, registry: &ProviderRegistry) -> io::Result<()> {
        let json = serde_json::to_string_pretty(registry).unwrap();
        write_atomic(path, &json)
    }
}
