//! Wire formats for the holepunch side-channel and the on-socket probe
//! protocol.
//!
//! Postcard-encoded. `MAGIC` is a 4-byte prefix on every on-socket probe
//! datagram so a [`quinn::AsyncUdpSocket`] wrapper can route probes
//! away from quinn without parsing further.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// First 4 bytes of every holepunch probe datagram. Probes are
/// recognized at the [`quinn::AsyncUdpSocket`] layer before quinn sees
/// them; quinn never has to disambiguate.
///
/// The trailing `1` is the wire-format version (ASCII). A future
/// breaking change to [`Probe`]'s layout bumps to `b"SnP2"` etc.; the
/// peer rejects unknown MAGICs (and we never accidentally decode a v1
/// probe as v2 because the prefix-match fails).
pub const MAGIC: [u8; 4] = *b"SnP1";

/// Per-(peer, session) probe datagram. Versioned via [`MAGIC`] — when
/// adding/removing fields, bump `MAGIC` so old peers reject the new
/// shape rather than postcard-misdecoding it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub session_id: [u8; 16],
    pub role: ProbeRole,
    pub sender_pk: [u8; 32],
    pub nonce: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeRole {
    Ping,
    Pong,
}

impl Probe {
    /// Encode this probe with the 4-byte MAGIC prefix.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        let body = postcard::to_allocvec(self)?;
        let mut out = Vec::with_capacity(MAGIC.len() + body.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decode a probe from a datagram that starts with MAGIC. Returns
    /// `Ok(None)` if the prefix doesn't match.
    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, postcard::Error> {
        if bytes.len() < MAGIC.len() || bytes[..MAGIC.len()] != MAGIC {
            return Ok(None);
        }
        let probe: Probe = postcard::from_bytes(&bytes[MAGIC.len()..])?;
        Ok(Some(probe))
    }
}

/// One side's candidate-address advertisement, sent over
/// [`sunset_sync::Signaler`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidates {
    pub session_id: [u8; 16],
    pub addresses: Vec<SocketAddr>,
    /// SHA-256 of the SubjectPublicKeyInfo for THIS side's QUIC server
    /// cert. The peer pins this hash to validate TLS regardless of CN.
    pub server_cert_sha256: [u8; 32],
}

/// Versioned wire enum carried inside
/// [`sunset_sync::SignalMessage::payload`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuicSignal {
    Candidates(Candidates),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_roundtrips_with_magic_prefix() {
        let p = Probe {
            session_id: [7u8; 16],
            role: ProbeRole::Ping,
            sender_pk: [9u8; 32],
            nonce: [3u8; 16],
        };
        let bytes = p.encode().unwrap();
        assert_eq!(&bytes[..4], &MAGIC);
        let back = Probe::decode(&bytes).unwrap();
        assert_eq!(back, Some(p));
    }

    #[test]
    fn probe_decode_rejects_non_magic() {
        let bytes = b"NOTQUIC".to_vec();
        let back = Probe::decode(&bytes).unwrap();
        assert_eq!(back, None);
    }

    #[test]
    fn probe_decode_short_bytes_is_none() {
        let back = Probe::decode(&[0u8; 2]).unwrap();
        assert_eq!(back, None);
    }

    #[test]
    fn candidates_roundtrip_through_quic_signal() {
        let c = Candidates {
            session_id: [1u8; 16],
            addresses: vec![
                "127.0.0.1:7777".parse().unwrap(),
                "[::1]:7778".parse().unwrap(),
            ],
            server_cert_sha256: [42u8; 32],
        };
        let bytes = postcard::to_allocvec(&QuicSignal::Candidates(c.clone())).unwrap();
        let back: QuicSignal = postcard::from_bytes(&bytes).unwrap();
        assert!(matches!(back, QuicSignal::Candidates(ref got) if got == &c));
    }
}
