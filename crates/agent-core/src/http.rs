//! Shared HTTP client construction. All outbound traffic (providers, MCP, OAuth) should use a
//! client from here so TLS, proxy and pooling behaviour is uniform.

use std::sync::Once;
use std::time::Duration;

use crate::config::product;
use crate::{Error, Result};

static INIT: Once = Once::new();

/// Installs the `ring` crypto provider for rustls. Idempotent; safe to call from tests.
pub fn init() {
    INIT.call_once(|| {
        // Ignore the error: another provider may already be installed by the host process.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn builder() -> reqwest::ClientBuilder {
    init();
    reqwest::Client::builder()
        .user_agent(format!("{}/{}", product().slug, product().version))
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(90))
}

/// Pooled client with product user agent. Per-request timeouts are set by callers.
/// Certificate verification uses the operating system's verifier and trust store
/// (rustls-platform-verifier), so certificates trusted in the macOS Keychain or
/// Windows certificate store are honoured.
pub fn client() -> reqwest::Result<reqwest::Client> {
    builder().build()
}

/// Client that skips TLS verification entirely. Equivalent to `curl -k`: the connection is
/// still encrypted but the peer is not authenticated, so it is only appropriate for endpoints
/// the user explicitly marks as self-signed (LAN servers etc.).
pub fn client_insecure() -> Result<reqwest::Client> {
    builder()
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| Error::Internal(e.to_string()))
}

/// Client that additionally trusts a specific PEM-encoded certificate (or PEM bundle), i.e.
/// a pinned self-signed server certificate or private CA for one provider.
pub fn client_with_extra_ca(pem: &str) -> Result<reqwest::Client> {
    let cert = reqwest::tls::Certificate::from_pem(pem.as_bytes())
        .map_err(|e| Error::InvalidInput(format!("certificate is not valid PEM: {e}")))?;
    builder()
        .add_root_certificate(cert)
        .build()
        .map_err(|e| Error::Internal(e.to_string()))
}
