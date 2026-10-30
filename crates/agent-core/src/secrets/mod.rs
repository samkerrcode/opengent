//! Secret storage. Secrets never live in SQLite, logs, or IPC responses; the core hands out
//! opaque `SecretRef`s and resolves them only when a request is being built.

#[cfg(feature = "native-secrets")]
mod native;

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[cfg(feature = "native-secrets")]
pub use native::NativeSecretStore;

use crate::{Error, Result};

/// Stable, non-sensitive handle to a stored secret (e.g. `provider/<uuid>/api_key`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export)]
pub struct SecretRef(pub String);

impl SecretRef {
    pub fn new(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        let valid = !key.is_empty()
            && key.len() <= 128
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'));
        if !valid {
            return Err(Error::InvalidInput(format!(
                "invalid secret reference `{key}`"
            )));
        }
        Ok(Self(key))
    }
}

pub trait SecretStore: Send + Sync + 'static {
    fn set(&self, key: &SecretRef, secret: &str) -> Result<()>;
    fn get(&self, key: &SecretRef) -> Result<Option<String>>;
    fn delete(&self, key: &SecretRef) -> Result<()>;
    /// Human-readable backend name for diagnostics ("macOS Keychain", "memory", ...).
    fn backend(&self) -> &'static str;
}

/// Non-persistent store for tests and for hosts where no native store is available.
#[derive(Default)]
pub struct MemorySecretStore {
    inner: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn set(&self, key: &SecretRef, secret: &str) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert(key.0.clone(), secret.to_owned());
        Ok(())
    }
    fn get(&self, key: &SecretRef) -> Result<Option<String>> {
        Ok(self.inner.lock().unwrap().get(&key.0).cloned())
    }
    fn delete(&self, key: &SecretRef) -> Result<()> {
        self.inner.lock().unwrap().remove(&key.0);
        Ok(())
    }
    fn backend(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_ref_validation() {
        assert!(SecretRef::new("provider/abc-123/api_key").is_ok());
        assert!(SecretRef::new("").is_err());
        assert!(SecretRef::new("has space").is_err());
        assert!(SecretRef::new("a".repeat(129)).is_err());
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemorySecretStore::default();
        let key = SecretRef::new("test/key").unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
        store.set(&key, "s3cret").unwrap();
        assert_eq!(store.get(&key).unwrap().as_deref(), Some("s3cret"));
        store.delete(&key).unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
    }
}
