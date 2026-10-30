//! OS credential store backend: macOS Keychain (login keychain) and Windows Credential
//! Manager. Entries are namespaced by the product identifier so renaming the display name
//! never orphans credentials.

use std::sync::{Mutex, OnceLock};

use keyring_core::{Entry, Error as KeyringError};

use super::{SecretRef, SecretStore};
use crate::{config::product, Error, Result};

pub struct NativeSecretStore {
    service: String,
    /// Native stores do not reliably sequence concurrent operations on one entry.
    op_lock: Mutex<()>,
}

static STORE_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

fn init_default_store() -> Result<()> {
    STORE_INIT
        .get_or_init(|| {
            #[cfg(target_os = "macos")]
            {
                let store = apple_native_keyring_store::keychain::Store::new()
                    .map_err(|e| e.to_string())?;
                keyring_core::set_default_store(store);
                Ok(())
            }
            #[cfg(target_os = "windows")]
            {
                let store =
                    windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
                keyring_core::set_default_store(store);
                Ok(())
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                Err("no native credential store is configured for this platform".to_string())
            }
        })
        .clone()
        .map_err(Error::Secrets)
}

impl NativeSecretStore {
    /// Uses the product identifier as the keychain "service"; `suffix` lets tests isolate.
    pub fn new() -> Result<Self> {
        Self::with_service(product().identifier.to_owned())
    }

    pub fn with_service(service: String) -> Result<Self> {
        init_default_store()?;
        Ok(Self {
            service,
            op_lock: Mutex::new(()),
        })
    }

    fn entry(&self, key: &SecretRef) -> Result<Entry> {
        Entry::new(&self.service, &key.0).map_err(|e| Error::Secrets(e.to_string()))
    }
}

impl SecretStore for NativeSecretStore {
    fn set(&self, key: &SecretRef, secret: &str) -> Result<()> {
        let _g = self.op_lock.lock().unwrap();
        self.entry(key)?
            .set_password(secret)
            .map_err(|e| Error::Secrets(e.to_string()))
    }

    fn get(&self, key: &SecretRef) -> Result<Option<String>> {
        let _g = self.op_lock.lock().unwrap();
        match self.entry(key)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(e) => Err(Error::Secrets(e.to_string())),
        }
    }

    fn delete(&self, key: &SecretRef) -> Result<()> {
        let _g = self.op_lock.lock().unwrap();
        match self.entry(key)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(KeyringError::NoEntry) => Ok(()),
            Err(e) => Err(Error::Secrets(e.to_string())),
        }
    }

    fn backend(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else if cfg!(target_os = "windows") {
            "Windows Credential Manager"
        } else {
            "unavailable"
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod tests {
    use super::*;

    /// Touches the real OS credential store. Run with `cargo test -- --ignored` on a machine
    /// where interactive keychain prompts are acceptable.
    #[test]
    #[ignore = "writes to the real OS credential store"]
    fn native_round_trip() {
        let store =
            NativeSecretStore::with_service(format!("{}.test", product().identifier)).unwrap();
        let key = SecretRef::new(format!("test/{}", uuid::Uuid::new_v4())).unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
        store.set(&key, "round-trip-value").unwrap();
        assert_eq!(
            store.get(&key).unwrap().as_deref(),
            Some("round-trip-value")
        );
        // 8 KiB payload approximates a large OAuth token set.
        let big = "x".repeat(8 * 1024);
        store.set(&key, &big).unwrap();
        assert_eq!(store.get(&key).unwrap().as_deref(), Some(big.as_str()));
        store.delete(&key).unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
        store.delete(&key).unwrap();
    }
}
