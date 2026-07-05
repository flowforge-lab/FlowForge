//! Backend-side secret storage for provider credentials.
//!
//! Provider secrets (bearer API keys, AWS secret access keys, session tokens)
//! live only on the backend — they never round-trip to the frontend, which sees
//! a single coarse `has_key` flag per connection. Production builds persist to
//! the OS keychain (macOS Data Protection Keychain, Windows Credential Manager,
//! Linux Secret Service). Test builds use an in-process map so CI stays hermetic
//! and never touches a real keychain.

use ff_core::SecretKind;
use std::sync::{Arc, OnceLock};

/// A pluggable secret backend addressed by an opaque account string that encodes
/// the connection id and secret kind.
trait SecretStore: Send + Sync {
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn get(&self, account: &str) -> Option<String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

#[cfg(not(test))]
const SERVICE: &str = "flowforge";

// ---------------------------------------------------------------------------
// macOS: Data Protection Keychain via security-framework (#727).
//
// The Data Protection Keychain (kSecUseDataProtectionKeychain) does NOT prompt
// the user for access — it grants access based on the app's code-signing team
// rather than per-binary identity, so dev rebuilds and updates never trigger a
// keychain password dialog. This replaces the previous `keyring` crate backend
// which used the legacy login.keychain and prompted on every signature change.
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
// Windows / Linux: keyring crate (unchanged).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
#[cfg(not(target_os = "macos"))]
struct KeyringStore;

#[cfg(not(test))]
#[cfg(not(target_os = "macos"))]
impl SecretStore for KeyringStore {
    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        keyring::Entry::new(SERVICE, account)
            .and_then(|entry| entry.set_password(value))
            .map_err(|e| e.to_string())
    }

    fn get(&self, account: &str) -> Option<String> {
        keyring::Entry::new(SERVICE, account)
            .ok()?
            .get_password()
            .ok()
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
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
            Arc::new(DataProtectionStore)
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
/// rather than persisted (#320). Values never leave the backend — presence only.
pub fn present(conn_id: &str) -> Vec<SecretKind> {
    SecretKind::ALL
        .into_iter()
        .filter(|k| get(conn_id, *k).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_clear_roundtrip() {
        let id = "conn-secrets-roundtrip";
        assert!(get(id, SecretKind::ApiKey).is_none());
        set(id, SecretKind::ApiKey, "sk-123").unwrap();
        assert_eq!(get(id, SecretKind::ApiKey).as_deref(), Some("sk-123"));
        clear(id, SecretKind::ApiKey).unwrap();
        assert!(get(id, SecretKind::ApiKey).is_none());
    }

    #[test]
    fn clearing_absent_secret_is_ok() {
        clear("conn-secrets-absent", SecretKind::SessionToken).unwrap();
    }

    #[test]
    fn present_reflects_stored_kinds_in_all_order() {
        let id = "conn-secrets-present";
        assert!(present(id).is_empty());
        set(id, SecretKind::SessionToken, "tok").unwrap();
        set(id, SecretKind::ApiKey, "sk").unwrap();
        // Returned in SecretKind::ALL order (ApiKey, SecretAccessKey, SessionToken),
        // not insertion order.
        assert_eq!(
            present(id),
            vec![SecretKind::ApiKey, SecretKind::SessionToken]
        );
        clear(id, SecretKind::ApiKey).unwrap();
        assert_eq!(present(id), vec![SecretKind::SessionToken]);
    }

    #[test]
    fn kinds_are_isolated_per_connection() {
        let id = "conn-secrets-isolated";
        set(id, SecretKind::SecretAccessKey, "aws-secret").unwrap();
        set(id, SecretKind::SessionToken, "aws-token").unwrap();
        assert_eq!(
            get(id, SecretKind::SecretAccessKey).as_deref(),
            Some("aws-secret")
        );
        assert_eq!(
            get(id, SecretKind::SessionToken).as_deref(),
            Some("aws-token")
        );
        assert!(get(id, SecretKind::ApiKey).is_none());
    }
}
