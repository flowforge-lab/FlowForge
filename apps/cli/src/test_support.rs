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

    /// Path `serve` reads its `[slack]` transport config from (#1060 T5).
    pub(crate) fn transports_path(&self) -> PathBuf {
        self.flowforge_dir.join("transports.toml")
    }

    /// Persist a registry at the env's `registry_path()` using the same
    /// atomic-rename primitive the production code uses.
    pub(crate) fn write_registry(&self, reg: &ProviderRegistry) {
        let json = serde_json::to_string_pretty(reg).expect("serialize");
        crate::registry::write_atomic(&self.registry_path(), &json).expect("atomic write");
    }

    /// Persist a `transports.toml` at the env's [`Self::transports_path`].
    pub(crate) fn write_transports(&self, toml: &str) {
        std::fs::create_dir_all(&self.flowforge_dir).expect("create flowforge dir");
        std::fs::write(self.transports_path(), toml).expect("write transports.toml");
    }
}

/// Run `f` with the Slack token vars removed, restoring the previous values
/// afterwards even on panic.
///
/// Mutating the environment is process-global, which is why this is funnelled
/// through one helper instead of open-coded per test: `scripts/test.sh` runs
/// nextest, whose process-per-test scheduler keeps these tests isolated from
/// each other, but doctests share a process — so the restore is what keeps
/// this honest rather than the scheduler.
pub(crate) fn with_env_unset<T>(f: impl FnOnce() -> T) -> T {
    with_env_set(&[], f)
}

/// RAII form of [`with_env_unset`] for `async` callers, which cannot run their
/// body inside a closure. Restores the previous environment on drop.
#[must_use = "the environment is restored when this guard drops, so dropping it \
              immediately makes the call a no-op"]
pub(crate) struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    pub(crate) fn unset() -> Self {
        let saved = managed_snapshot();
        // SAFETY: restored in `Drop`. See the note on `with_env_set`.
        unsafe {
            for (key, _) in &saved {
                std::env::remove_var(key);
            }
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: mirrors the mutation performed in `unset`.
        unsafe {
            for (key, value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn managed_snapshot() -> Vec<(&'static str, Option<String>)> {
    const MANAGED: [&str; 2] = [crate::serve::APP_TOKEN_VAR, crate::serve::BOT_TOKEN_VAR];
    MANAGED
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect()
}

/// Run `f` with `vars` set (and any Slack token var *not* listed removed),
/// restoring the previous environment afterwards even on panic.
pub(crate) fn with_env_set<T>(vars: &[(&str, &str)], f: impl FnOnce() -> T) -> T {
    let saved = managed_snapshot();

    // SAFETY: single-threaded mutation of this process's environment, restored
    // below. See the module note above on why isolation is not relied upon.
    unsafe {
        for (key, _) in &saved {
            std::env::remove_var(key);
        }
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    unsafe {
        for (key, value) in saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}
