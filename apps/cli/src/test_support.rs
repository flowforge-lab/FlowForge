//! Test-only helpers shared by the `config` and `secrets` integration tests.
//!
//! `TestEnv` is a self-contained tempdir that emulates the config directory
//! layout the production code reads (`<config_dir>/flowforge/...`). Tests
//! install a thread-local config-dir override for the duration of their
//! body, so the runner's `dirs::config_dir()` calls resolve to the tempdir
//! instead of the user's real config dir.
//!
//! `MEM_STORE_LOCK` serializes tests that share the in-process `MemStore` (a
//! `cfg(test)`-only keychain stand-in in [`crate::secrets`]). The store is
//! process-global; without this guard, parallel threads can see each other's
//! secret entries.

#![cfg(test)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;

use ff_core::ProviderRegistry;

/// Serializes all `secrets::*` integration tests in this crate. The
/// in-process `MemStore` is a `OnceLock`, so concurrent test threads can
/// otherwise read each other's accounts. A `std::sync::Mutex` poisons on
/// panic, so we wrap it in `lock_or_recover` to keep a panic in one test
/// from poisoning every sibling.
pub static MEM_STORE_LOCK: Mutex<()> = Mutex::new(());

/// Lock `MEM_STORE_LOCK`, recovering from poison (which is fine — the guard
/// is a `()` with no shared state to corrupt).
pub(crate) fn lock_mem_store() -> std::sync::MutexGuard<'static, ()> {
    MEM_STORE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// Per-thread config-dir override. `None` (the default) means "use
// `dirs::config_dir()`". `TestEnv::install` sets this to its tempdir for
// the lifetime of the test body. The override is thread-local so parallel
// test threads cannot pollute each other; tests in the same thread see
// the most recent install until the `InstallGuard` drops.
thread_local! {
    static CONFIG_DIR_OVERRIDE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Reader for the override; called by `registry::config_dir` under `cfg(test)`.
pub(crate) fn config_dir_override() -> Option<PathBuf> {
    CONFIG_DIR_OVERRIDE.with(|c| c.borrow().clone())
}

/// A self-contained `<config_dir>` stand-in: `tmp/<random>/flowforge/`.
///
/// On `install()`, points the runner's `config_dir()` at the tempdir's
/// `flowforge/` subdir for the lifetime of the returned `InstallGuard`.
/// Drop the guard (or call `restore()`) to revert to the real config dir.
pub(crate) struct TestEnv {
    _dir: tempfile::TempDir,
    flowforge_dir: PathBuf,
    _guard: Option<InstallGuard>,
}

struct InstallGuard {
    previous: Option<PathBuf>,
}

impl InstallGuard {
    fn install(path: PathBuf) -> Self {
        let previous = CONFIG_DIR_OVERRIDE.with(|c| c.replace(Some(path)));
        Self { previous }
    }

    fn restore(&self) {
        CONFIG_DIR_OVERRIDE.with(|c| *c.borrow_mut() = self.previous.clone());
    }
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

impl TestEnv {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let flowforge_dir = dir.path().join("flowforge");
        std::fs::create_dir_all(&flowforge_dir).expect("mkdir flowforge");
        // The override here is the *config dir* (i.e. `tempdir`), not the
        // `flowforge` subdir — `registry::registry_path()` appends
        // `flowforge/provider-registry.json` to whatever `config_dir()`
        // returns, so the override must be at the level above.
        let guard = InstallGuard::install(dir.path().to_path_buf());
        Self {
            _dir: dir,
            flowforge_dir,
            _guard: Some(guard),
        }
    }

    pub(crate) fn registry_path(&self) -> PathBuf {
        self.flowforge_dir.join("provider-registry.json")
    }

    pub(crate) fn legacy_path(&self) -> PathBuf {
        self.flowforge_dir.join("provider.json")
    }

    /// Persist a registry at the env's `registry_path()` using the same
    /// atomic-rename primitive the production code uses.
    pub(crate) fn write_registry(&self, reg: &ProviderRegistry) {
        let json = serde_json::to_string_pretty(reg).expect("serialize");
        crate::registry::write_atomic(&self.registry_path(), &json).expect("atomic write");
    }
}
