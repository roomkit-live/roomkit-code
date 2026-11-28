//! The HTTP client of the application's own requests: model discovery and
//! update checks.
//!
//! reqwest 0.13 has no ring-backed rustls feature, so each client is given a
//! rustls configuration with the ring provider and the webpki roots, as the
//! RoomKit and tool clients are: the same trust on every host, and no system
//! TLS library. A client built without it panics, no provider being installed.
//! roomkit-tools enables reqwest's gzip for the whole build; these clients
//! ask for uncompressed answers all the same.

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

/// What a connection that would send a credential unencrypted is told.
pub(crate) const PLAINTEXT_CREDENTIAL: &str = "This http:// address would send the credential unencrypted to a host outside this machine and its local network; use an https:// address";

/// Whether `address` is plain HTTP to a host outside this machine and its
/// local network: a credential sent there crosses networks unencrypted. A
/// host is local when it is `localhost`, a single-label name, a name under
/// `.local`, `.internal`, `.lan`, `.home.arpa` or `.ts.net`, or a loopback,
/// unspecified, private, shared (100.64.0.0/10), link-local or unique-local
/// address. An address that does not read is
/// not this check's to refuse.
pub(crate) fn plaintext_to_remote(address: &str) -> bool {
    let Ok(url) = url::Url::parse(address) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => {
            // 100.64.0.0/10, the shared range overlay networks such as
            // Tailscale address their peers in.
            let [first, second, ..] = ip.octets();
            let shared = first == 100 && (64..128).contains(&second);
            !(ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || shared)
        }
        Some(url::Host::Ipv6(ip)) => {
            let first = ip.segments()[0];
            let unique_local = first & 0xfe00 == 0xfc00;
            let link_local = first & 0xffc0 == 0xfe80;
            !(ip.is_loopback() || unique_local || link_local)
        }
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            let local_suffix = [
                ".localhost",
                ".local",
                ".internal",
                ".lan",
                ".home.arpa",
                // A tailnet's names, reached over its encrypted tunnel.
                ".ts.net",
            ]
            .iter()
            .any(|suffix| name.ends_with(suffix));
            !(name == "localhost" || !name.contains('.') || local_suffix)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::plaintext_to_remote;

    #[test]
    fn only_plain_http_to_a_host_outside_the_local_network_is_remote() {
        for local in [
            "http://127.0.0.1:8000/v1",
            "http://localhost:11434/v1",
            "http://[::1]:8000/v1",
            "http://192.168.1.20:8000/v1",
            "http://10.0.0.4/v1",
            "http://gpu-box:8000/v1",
            "http://gpu.local/v1",
            "http://[fd12:3456::1]/v1",
            "http://100.101.102.103:8000/v1",
            "http://0.0.0.0:8000/v1",
            "http://gpu.tail1234.ts.net/v1",
            "https://api.example.com/v1",
        ] {
            assert!(!plaintext_to_remote(local), "{local}");
        }
        for remote in [
            "http://api.example.com/v1",
            "http://8.8.8.8/v1",
            "http://[2001:db8::1]/v1",
            "http://172.32.0.1/v1",
            "http://100.128.0.1/v1",
        ] {
            assert!(plaintext_to_remote(remote), "{remote}");
        }
    }
}
