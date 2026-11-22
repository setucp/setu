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

/// Describe a DER certificate (the leaf the server sent).
#[must_use]
pub fn describe(der: &[u8]) -> CertOutput {
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
    CertOutput {
        served: true,
        issuer,
        not_before: Some(validity.not_before.timestamp()),
        not_after: Some(validity.not_after.timestamp()),
        public: !self_signed && !local_ca,
        error: None,
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
            .map(|c| describe(c.as_ref()))
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
        let out = describe(b"not a certificate");
        assert!(!out.served);
        assert!(out.error.is_some());
    }

    #[tokio::test]
    async fn a_closed_port_is_an_unserved_result() {
        let out = probe("127.0.0.1:1", "example.com").await;
        assert!(!out.served);
        assert!(out.error.unwrap().contains("cannot reach"));
    }

    #[tokio::test]
    async fn a_self_signed_certificate_is_read_and_not_public() {
        use rustls::pki_types::PrivateKeyDer;
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["site.example".to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("cert");
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
        let out = probe(&addr, "site.example").await;
        assert!(out.served, "{out:?}");
        assert!(!out.public, "self-signed is not public");
        assert!(out.not_before.unwrap() <= out.not_after.unwrap());
    }
}
