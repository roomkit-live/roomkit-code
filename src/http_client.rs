//! The HTTP client of the application's own requests: model discovery and
//! update checks.
//!
//! reqwest 0.13 has no ring-backed rustls feature, so each client is given a
//! rustls configuration with the ring provider and the webpki roots, as the
//! RoomKit and tool clients are: the same trust on every host, and no system
//! TLS library. A client built without it panics, no provider being installed.
//! roomkit-tools enables reqwest's gzip; these clients still ask for
//! uncompressed answers, as before.

use std::sync::Arc;

/// A client builder with the application's TLS configuration.
pub(crate) fn builder() -> Result<reqwest::ClientBuilder, rustls::Error> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    // HTTP/1.1 by ALPN, as reqwest's own rustls configuration offers it.
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .no_gzip())
}
