//! `CertStatus`: read the certificate Caddy serves for a name, with a TLS
//! handshake to the local HTTPS listener using that name as the server
//! name. Read-only; nothing is verified (the point is to see what is
//! served, including a certificate that would not be trusted) and no
//! data is sent after the handshake.

use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use setu_proto::CertOutput;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use x509_parser::prelude::{FromDer, X509Certificate};

/// Where Caddy listens for HTTPS.
pub const LOCAL_HTTPS: &str = "127.0.0.1:443";
const TIMEOUT: Duration = Duration::from_secs(10);
/// Caddy's own CA signs certificates for names it cannot get a public one
/// for; browsers do not trust them.
const CADDY_LOCAL_CA: &str = "Caddy Local Authority";
/// Let's Encrypt's staging issuers carry this in their name.
const STAGING_MARK: &str = "(STAGING)";

#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn unserved(error: impl Into<String>) -> CertOutput {
    CertOutput {
        error: Some(error.into()),
        ..CertOutput::default()
    }
}

/// The DNS names in the leaf's subjectAltName.
fn dns_names(cert: &X509Certificate<'_>) -> Vec<String> {
    use x509_parser::extensions::GeneralName;
    cert.subject_alternative_name()
        .ok()
        .flatten()
        .map(|san| {
            san.value
                .general_names
                .iter()
                .filter_map(|n| match n {
                    GeneralName::DNSName(d) => Some((*d).to_ascii_lowercase()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the certificate name `pattern` covers `domain` (a `*.` wildcard
/// stands for exactly one label).
fn name_covers(pattern: &str, domain: &str) -> bool {
    let domain = domain.to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(rest) => domain
            .split_once('.')
            .is_some_and(|(label, tail)| !label.is_empty() && tail == rest),
        None => pattern == domain,
    }
}

/// Describe a DER certificate (the leaf the server sent) as the certificate
/// of `domain`: one whose names do not cover `domain` (a default or
/// fallback certificate) or that a staging CA issued is not public.
#[must_use]
pub fn describe(der: &[u8], domain: &str) -> CertOutput {
    let Ok((_, cert)) = X509Certificate::from_der(der) else {
        return unserved("the served certificate could not be parsed");
    };
    let issuer_name = |n: &x509_parser::x509::X509Name<'_>| {
        n.iter_common_name()
            .chain(n.iter_organization())
            .find_map(|a| a.as_str().ok())
            .map(str::to_string)
    };
    let issuer = issuer_name(cert.issuer());
    let self_signed = cert.issuer() == cert.subject();
    let local_ca = cert.issuer().to_string().contains(CADDY_LOCAL_CA);
    let validity = cert.validity();
    let names = dns_names(&cert);
    let covers = names.iter().any(|n| name_covers(n, domain));
    let staging = cert.issuer().to_string().contains(STAGING_MARK);
    let error = if !covers {
        Some(format!(
            "the served certificate is for {}, not for {domain}",
            if names.is_empty() {
                "no DNS name".to_string()
            } else {
                names.join(", ")
            }
        ))
    } else if staging {
        Some("the served certificate is from a staging CA, which browsers do not trust".to_string())
    } else {
        None
    };
    CertOutput {
        served: true,
        issuer,
        not_before: Some(validity.not_before.timestamp()),
        not_after: Some(validity.not_after.timestamp()),
        public: !self_signed && !local_ca && covers && !staging,
        error,
    }
}

/// Handshake with `addr` using `domain` as the server name.
pub async fn probe(addr: &str, domain: &str) -> CertOutput {
    let Ok(name) = ServerName::try_from(domain.to_string()) else {
        return unserved("not a valid server name");
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = match ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
    {
        Ok(b) => b
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
            .with_no_client_auth(),
        Err(e) => return unserved(e.to_string()),
    };
    let attempt = async {
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("cannot reach the local HTTPS listener: {e}"))?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .map_err(|e| format!("no certificate served: {e}"))?;
        let (_, conn) = tls.get_ref();
        conn.peer_certificates()
            .and_then(<[_]>::first)
            .map(|c| describe(c.as_ref(), domain))
            .ok_or_else(|| "the server sent no certificate".to_string())
    };
    match tokio::time::timeout(TIMEOUT, attempt).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => unserved(e),
        Err(_) => unserved("timed out waiting for the TLS handshake"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_reported_not_panicked() {
        let out = describe(b"not a certificate", "site.example");
        assert!(!out.served);
        assert!(out.error.is_some());
    }

    #[tokio::test]
    async fn a_closed_port_is_an_unserved_result() {
        let out = probe("127.0.0.1:1", "example.com").await;
        assert!(!out.served);
        assert!(out.error.unwrap().contains("cannot reach"));
    }

    /// Serve one handshake with `cert` and return the probe of `domain`.
    async fn probe_served(
        cert: &rcgen::Certificate,
        key: &rcgen::KeyPair,
        domain: &str,
    ) -> CertOutput {
        use rustls::pki_types::PrivateKeyDer;
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivateKeyDer::try_from(key.serialize_der()).expect("key der"),
        )
        .expect("server config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let _ = acceptor.accept(tcp).await;
        });
        probe(&addr, domain).await
    }

    fn self_signed(name: &str) -> (rcgen::Certificate, rcgen::KeyPair) {
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec![name.to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("cert");
        (cert, key)
    }

    /// A leaf for `names` signed by a CA called `ca_name` (not Caddy's).
    fn signed_leaf(ca_name: &str, names: &[&str]) -> (rcgen::Certificate, rcgen::KeyPair) {
        let ca_key = rcgen::KeyPair::generate().expect("ca key");
        let mut ca_params = rcgen::CertificateParams::new(Vec::new()).expect("ca params");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, ca_name);
        let ca = ca_params.self_signed(&ca_key).expect("ca");
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(
            names.iter().map(|n| (*n).to_string()).collect::<Vec<_>>(),
        )
        .expect("params")
        .signed_by(&key, &ca, &ca_key)
        .expect("leaf");
        (cert, key)
    }

    #[tokio::test]
    async fn a_self_signed_certificate_is_read_and_not_public() {
        let (cert, key) = self_signed("site.example");
        let out = probe_served(&cert, &key, "site.example").await;
        assert!(out.served, "{out:?}");
        assert!(!out.public, "self-signed is not public");
        assert!(out.not_before.unwrap() <= out.not_after.unwrap());
    }

    #[tokio::test]
    async fn a_leaf_from_another_ca_for_the_name_is_public() {
        let (cert, key) = signed_leaf("Test Public CA", &["site.example"]);
        let out = probe_served(&cert, &key, "site.example").await;
        assert!(out.public, "{out:?}");
        assert!(out.error.is_none());
    }

    #[tokio::test]
    async fn a_certificate_for_another_name_is_not_public_and_names_its_names() {
        let (cert, key) = signed_leaf("Test Public CA", &["other.example"]);
        let out = probe_served(&cert, &key, "site.example").await;
        assert!(out.served);
        assert!(!out.public, "{out:?}");
        let error = out.error.expect("error");
        assert!(
            error.contains("other.example") && error.contains("site.example"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_staging_issuer_is_not_public() {
        let (cert, key) = signed_leaf("(STAGING) Pretend Pear X1", &["site.example"]);
        let out = probe_served(&cert, &key, "site.example").await;
        assert!(!out.public, "{out:?}");
        assert!(out.error.expect("error").contains("staging"));
    }

    #[test]
    fn a_wildcard_covers_one_label() {
        assert!(name_covers("*.example.com", "a.example.com"));
        assert!(!name_covers("*.example.com", "example.com"));
        assert!(!name_covers("*.example.com", "a.b.example.com"));
        assert!(name_covers("site.example", "SITE.example"));
    }
}
