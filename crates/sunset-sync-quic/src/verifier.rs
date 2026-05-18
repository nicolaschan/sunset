//! rustls `ServerCertVerifier` that pins on the leaf cert's SHA-256
//! digest. Used by the QUIC client side to accept the peer's
//! self-signed cert without WebPKI.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct PinnedCertVerifier {
    expected_sha256: [u8; 32],
    supported_algorithms: WebPkiSupportedAlgorithms,
}

impl PinnedCertVerifier {
    pub fn new(expected_sha256: [u8; 32]) -> Arc<Self> {
        Arc::new(Self {
            expected_sha256,
            supported_algorithms: rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        })
    }
}

impl ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer,
        _intermediates: &[CertificateDer],
        _server_name: &ServerName,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut hasher = Sha256::new();
        hasher.update(end_entity.as_ref());
        let digest: [u8; 32] = hasher.finalize().into();
        if digest == self.expected_sha256 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::CertificateError::UnknownIssuer.into())
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.supported_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.supported_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algorithms.supported_schemes()
    }
}
