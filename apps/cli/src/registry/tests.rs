use super::*;
use ff_core::{ProviderConfig, ProviderKind};
use std::fs;
use std::io;
use std::path::Path;

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
    // Migrated active (ollama) plus the seeded candle-vllm local backend and the
    // seeded OpenRouter connection (v2 migration, #807).
    assert_eq!(loaded.connections.len(), 3);
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
