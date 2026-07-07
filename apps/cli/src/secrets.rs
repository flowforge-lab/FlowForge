//! Backend-side secret storage for provider credentials (#724).
//!
//! Provider secrets (bearer API keys, AWS secret access keys, session tokens)
//! live only on the backend — they never appear in argv or in registry JSON.
//! Production builds persist to the OS keychain (macOS Data Protection Keychain
//! with a keyring fallback, Windows Credential Manager, Linux Secret Service).
//! Test builds use an in-process map so CI stays hermetic and never touches a
//! real keychain.
//!
//! This module is a slim port of `apps/desktop/src-tauri/src/secrets.rs`: same
//! `SecretStore` trait, same `SERVICE` name, same `{conn_id}:{slug}` account
//! scheme, same `errSecItemNotFound` → `Ok(())` mapping. The desktop owns the
//! IPC commands; the CLI uses this directly from its `config` subcommand.

use ff_core::SecretKind;
use std::sync::{Arc, OnceLock};

/// A pluggable secret backend addressed by an opaque account string that encodes
/// the connection id and secret kind. Mirrors the desktop's trait so the two
/// halves of the product share one account layout.
trait SecretStore: Send + Sync {
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn get(&self, account: &str) -> Option<String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

#[cfg(not(test))]
const SERVICE: &str = "flowforge";

// ---------------------------------------------------------------------------
// macOS: Data Protection Keychain via security-framework.
//
// The Data Protection Keychain (kSecUseDataProtectionKeychain) does NOT prompt
// the user for access — it grants access based on the app's code-signing team
// rather than per-binary identity, so dev rebuilds and updates never trigger a
// keychain password dialog. However, Data Protection requires a real Apple
// code-signing identity with keychain-access-groups entitlement; ad-hoc signed
// dev builds get errSecMissingEntitlement (-34018). The macOS fallback wraps
// Data Protection around the keyring crate (legacy login keychain) and degrades
// transparently, mirroring the desktop's behavior.
// ---------------------------------------------------------------------------

#[cfg(not(test))]
#[cfg(target_os = "macos")]
struct DataProtectionStore;

#[cfg(not(test))]
#[cfg(target_os = "macos")]
impl SecretStore for DataProtectionStore {
    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        use security_framework::passwords::{
            delete_generic_password_options, set_generic_password_options, PasswordOptions,
        };
        // Data Protection Keychain does not support "update in place" like the
        // legacy keychain — SecItemAdd returns errSecDuplicateItem if the entry
        // exists. Delete first (idempotent), then add.
        let mut del_opts = PasswordOptions::new_generic_password(SERVICE, account);
        del_opts.use_protected_keychain();
        let _ = delete_generic_password_options(del_opts); // ignore NotFound

        let mut opts = PasswordOptions::new_generic_password(SERVICE, account);
        opts.use_protected_keychain();
        set_generic_password_options(value.as_bytes(), opts).map_err(|e| e.to_string())
    }

    fn get(&self, account: &str) -> Option<String> {
        use security_framework::passwords::{generic_password, PasswordOptions};
        let mut opts = PasswordOptions::new_generic_password(SERVICE, account);
        opts.use_protected_keychain();
        generic_password(opts)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        use security_framework::passwords::{delete_generic_password_options, PasswordOptions};
        let mut opts = PasswordOptions::new_generic_password(SERVICE, account);
        opts.use_protected_keychain();
        match delete_generic_password_options(opts) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()), // errSecItemNotFound
            Err(e) => Err(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// keyring crate: legacy login keychain on macOS, Credential Manager on Windows,
// Secret Service on Linux. Works with ad-hoc signed builds but may prompt.
// ---------------------------------------------------------------------------

#[cfg(not(test))]
struct KeyringStore;

#[cfg(not(test))]
impl SecretStore for KeyringStore {
    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        keyring::Entry::new(SERVICE, account)
            .and_then(|entry| entry.set_password(value))
            .map_err(|e| e.to_string())
    }

    fn get(&self, account: &str) -> Option<String> {
        keyring::Entry::new(SERVICE, account)
            .ok()
            .and_then(|entry| entry.get_password().ok())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// macOS fallback: try Data Protection first, fall back to keyring.
// Production builds (Apple Developer cert) use Data Protection silently.
// Dev builds (ad-hoc) automatically degrade to keyring (legacy login keychain).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
#[cfg(target_os = "macos")]
struct FallbackStore {
    primary: DataProtectionStore,
    fallback: KeyringStore,
}

#[cfg(not(test))]
#[cfg(target_os = "macos")]
impl SecretStore for FallbackStore {
    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        match self.primary.set(account, value) {
            Ok(()) => Ok(()),
            Err(_) => self.fallback.set(account, value),
        }
    }

    fn get(&self, account: &str) -> Option<String> {
        self.primary
            .get(account)
            .or_else(|| self.fallback.get(account))
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        // Best-effort cleanup from both backends.
        let _ = self.primary.delete(account);
        let _ = self.fallback.delete(account);
        Ok(())
    }
}

/// In-process backend used under `cfg(test)`; keeps CI off the real keychain.
#[cfg(test)]
#[derive(Default)]
struct MemStore {
    map: std::sync::RwLock<std::collections::HashMap<String, String>>,
}

#[cfg(test)]
impl SecretStore for MemStore {
    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.map
            .write()
            .unwrap()
            .insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, account: &str) -> Option<String> {
        self.map.read().unwrap().get(account).cloned()
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.map.write().unwrap().remove(account);
        Ok(())
    }
}

fn store() -> &'static Arc<dyn SecretStore> {
    static STORE: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();
    STORE.get_or_init(|| {
        #[cfg(test)]
        {
            Arc::new(MemStore::default())
        }
        #[cfg(not(test))]
        #[cfg(target_os = "macos")]
        {
            Arc::new(FallbackStore {
                primary: DataProtectionStore,
                fallback: KeyringStore,
            })
        }
        #[cfg(not(test))]
        #[cfg(not(target_os = "macos"))]
        {
            Arc::new(KeyringStore)
        }
    })
}

/// Account key for a `(connection, secret kind)` pair. Stable across runs so a
/// stored secret is recoverable by connection id.
fn account(conn_id: &str, kind: SecretKind) -> String {
    format!("{conn_id}:{}", kind.slug())
}

/// Store `value` as the secret of `kind` for `conn_id`, overwriting any existing.
pub fn set(conn_id: &str, kind: SecretKind, value: &str) -> Result<(), String> {
    store().set(&account(conn_id, kind), value)
}

/// Fetch the stored secret of `kind` for `conn_id`, if any.
pub fn get(conn_id: &str, kind: SecretKind) -> Option<String> {
    store().get(&account(conn_id, kind))
}

/// Remove the secret of `kind` for `conn_id`. Idempotent — clearing an absent
/// secret succeeds.
pub fn clear(conn_id: &str, kind: SecretKind) -> Result<(), String> {
    store().delete(&account(conn_id, kind))
}

/// The secret kinds currently stored for `conn_id`, in [`SecretKind::ALL`] order.
/// The keychain is the single source of truth, so this is recomputed on demand
/// rather than persisted. Values never leave the backend — presence only.
pub fn present(conn_id: &str) -> Vec<SecretKind> {
    SecretKind::ALL
        .into_iter()
        .filter(|k| get(conn_id, *k).is_some())
        .collect()
}

#[cfg(test)]
mod tests;
