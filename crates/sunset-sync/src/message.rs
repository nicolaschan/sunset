//! Wire-protocol messages exchanged between sunset-sync peers.
//!
//! All messages are postcard-encoded. The transport carries one whole
//! `SyncMessage` per `recv_reliable` / `send_reliable` call.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use sunset_store::{ContentBlock, Filter, Hash, SignedKvEntry, VerifyingKey};

use crate::error::{Error, Result};
use crate::types::PeerId;

/// A digest range for `DigestExchange`. v1 supports only `All` — the digest
/// covers every entry matching the filter, no partitioning. Future variants
/// (hash-prefix buckets, sequence-number ranges, hybrid) can be added
/// without breaking older peers because postcard tolerates new enum
/// variants on read by erroring at decode time, which the receiver maps to
/// `Error::Decode("unknown DigestRange")`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DigestRange {
    All,
}

/// Wire message types. Notably absent: `SubscribeRequest` — subscriptions
/// are KV entries that propagate via `EventDelivery` like any other event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMessage {
    Hello {
        protocol_version: u32,
        peer_id: PeerId,
    },
    EventDelivery {
        entries: Vec<SignedKvEntry>,
        blobs: Vec<ContentBlock>,
    },
    BlobRequest {
        hash: Hash,
    },
    BlobResponse {
        block: ContentBlock,
    },
    DigestExchange {
        filter: Filter,
        range: DigestRange,
        bloom: Bytes,
    },
    Fetch {
        entries: Vec<(VerifyingKey, Bytes)>,
    },
    EphemeralDelivery {
        datagram: sunset_store::SignedDatagram,
    },
    Goodbye {},
}

impl SyncMessage {
    pub fn encode(&self) -> Result<Bytes> {
        postcard::to_stdvec(self)
            .map(Bytes::from)
            .map_err(|e| Error::Decode(format!("encode: {e}")))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        postcard::from_bytes(bytes).map_err(|e| Error::Decode(format!("decode: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    #[test]
    fn hello_postcard_roundtrip() {
        let m = SyncMessage::Hello {
            protocol_version: 1,
            peer_id: PeerId(vk(b"alice")),
        };
        let encoded = m.encode().unwrap();
        let decoded = SyncMessage::decode(&encoded).unwrap();
        assert_eq!(m, decoded);
    }

    #[test]
    fn digest_exchange_postcard_roundtrip() {
        let m = SyncMessage::DigestExchange {
            filter: Filter::Keyspace(vk(b"alice")),
            range: DigestRange::All,
            bloom: Bytes::from_static(&[0xff; 32]),
        };
        let encoded = m.encode().unwrap();
        let decoded = SyncMessage::decode(&encoded).unwrap();
        assert_eq!(m, decoded);
    }

    #[test]
    fn goodbye_postcard_roundtrip() {
        let m = SyncMessage::Goodbye {};
        let encoded = m.encode().unwrap();
        assert_eq!(SyncMessage::decode(&encoded).unwrap(), m);
    }

    #[test]
    fn decode_garbage_returns_decode_error() {
        let err = SyncMessage::decode(&[0xff, 0xff, 0xff, 0xff]).unwrap_err();
        assert!(matches!(err, Error::Decode(_)));
    }

    #[test]
    fn ephemeral_delivery_postcard_roundtrip() {
        use sunset_store::SignedDatagram;
        let m = SyncMessage::EphemeralDelivery {
            datagram: SignedDatagram {
                verifying_key: vk(b"alice"),
                name: Bytes::from_static(b"room/voice/alice/0042"),
                payload: Bytes::from_static(b"opus-frame-bytes"),
                signature: Bytes::from_static(&[0xab; 64]),
            },
        };
        let encoded = m.encode().unwrap();
        let decoded = SyncMessage::decode(&encoded).unwrap();
        assert_eq!(m, decoded);
    }

    /// Frozen wire-format vector for SyncMessage::EphemeralDelivery.
    /// If this hex changes, every existing peer breaks — bump the wire
    /// format version, don't fix the test.
    #[test]
    fn ephemeral_delivery_frozen_vector() {
        use sunset_store::SignedDatagram;
        let m = SyncMessage::EphemeralDelivery {
            datagram: SignedDatagram {
                verifying_key: vk(b"alice"),
                name: Bytes::from_static(b"room/voice/alice/0042"),
                payload: Bytes::from_static(b"opus-frame-bytes"),
                signature: Bytes::from_static(&[0xab; 64]),
            },
        };
        let encoded = m.encode().unwrap();
        let digest = blake3::hash(&encoded);
        assert_eq!(
            digest.to_hex().as_str(),
            "313e80dd346c26e06cc1cc6288a1446b87a162dce603061ae97194cc44bbe019",
            "If this fails the EphemeralDelivery wire format has drifted — DO NOT update this hex without bumping the wire-format version.",
        );
    }
}
