//! Shared HTTP client construction. All outbound traffic (providers, MCP, OAuth) should use a
//! client from here so TLS, proxy and pooling behaviour is uniform.

use std::sync::Once;
use std::time::Duration;

use crate::config::product;

static INIT: Once = Once::new();

/// Installs the `ring` crypto provider for rustls. Idempotent; safe to call from tests.
pub fn init() {
    INIT.call_once(|| {
        // Ignore the error: another provider may already be installed by the host process.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Pooled client with product user agent. Per-request timeouts are set by callers.
pub fn client() -> reqwest::Result<reqwest::Client> {
    init();
    reqwest::Client::builder()
        .user_agent(format!("{}/{}", product().slug, product().version))
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
}
