//! Per-process self-signed QUIC server cert + its SHA-256 hash.
//! The hash is shared via the signaler so the peer can pin it for
//! rustls verification regardless of CN/SAN.

use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum CertError {
    #[error("rcgen: {0}")]
    Rcgen(String),
}

/// One generated self-signed cert and its DER-encoded private key.
#[derive(Clone)]
pub struct SelfSignedCert {
    pub cert_der: Vec<u8>,
    pub private_key_der: Vec<u8>,
    /// SHA-256 of the leaf cert DER. Matches the wtransport
    /// `ServerHashVerification` model: clients pin this hash and
    /// reject any cert whose digest doesn't match.
    pub cert_sha256: [u8; 32],
}

/// Generate a fresh self-signed cert for SNI `"sunset"`. The cert is
/// only ever validated by hash pinning on the peer side — CN/SAN
/// values don't matter for our use.
pub fn generate() -> Result<SelfSignedCert, CertError> {
    let cert = rcgen::generate_simple_self_signed(vec!["sunset".to_string()])
        .map_err(|e| CertError::Rcgen(e.to_string()))?;
    let cert_der = cert.cert.der().to_vec();
    let private_key_der = cert.signing_key.serialize_der();
    let mut hasher = Sha256::new();
    hasher.update(&cert_der);
    let cert_sha256: [u8; 32] = hasher.finalize().into();
    Ok(SelfSignedCert {
        cert_der,
        private_key_der,
        cert_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_distinct_certs() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a.cert_der, b.cert_der);
        assert_ne!(a.cert_sha256, b.cert_sha256);
    }

    #[test]
    fn cert_sha256_matches_independent_recompute() {
        let c = generate().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&c.cert_der);
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(c.cert_sha256, expected);
    }
}
