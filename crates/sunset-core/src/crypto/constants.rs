//! Cryptographic constants. Every literal here is part of the v1 wire
//! format. Changing any of them invalidates every key, signature, and
//! ciphertext ever produced under v1 — bump the wire-format version
//! before touching them.

use argon2::Params;

/// 32-byte salt fed into Argon2id when deriving `K_room` from a room name.
/// Right-padded with NUL to 32 bytes (the `argon2` crate accepts arbitrary
/// salt bytes; we fix the length to keep the constant pinnable).
pub const ROOM_KEY_SALT: &[u8; 32] = b"sunset-chat-v1-room\0\0\0\0\0\0\0\0\0\0\0\0\0";

/// Domain-separation input for the blake3-keyed `room_fingerprint`.
pub const FINGERPRINT_DOMAIN: &[u8] = b"sunset-chat-v1-fingerprint";

/// HKDF `info` for deriving the open-room `K_epoch_0` from `K_room`.
pub const EPOCH_0_DOMAIN: &[u8] = b"sunset-chat-v1-epoch-0";

/// HKDF `info` *prefix* for deriving a per-message AEAD key from an epoch
/// root. Per-message info is `MSG_KEY_DOMAIN || epoch_id_le_bytes || value_hash`.
pub const MSG_KEY_DOMAIN: &[u8] = b"sunset-chat-v1-msg";

/// AEAD additional-data prefix bound to every message ciphertext. The full
/// AD is `MSG_AAD_DOMAIN || room_fingerprint || epoch_id_le_bytes || sender_id || sent_at_ms_le_bytes`.
pub const MSG_AAD_DOMAIN: &[u8] = b"sunset-chat-v1-msg-aad";

/// Production Argon2id parameters: m=19 MiB, t=2, p=1, 32-byte output.
/// Matches OWASP 2023 baseline.
///
/// `Params::new` returns `Result`, so this is a function rather than a const.
pub fn production_params() -> Params {
    Params::new(19_456, 2, 1, Some(32)).expect("Argon2id production parameters are valid")
}

/// Test parameters tuned for sub-millisecond derivation: m=8 KiB, t=1, p=1.
/// **Never use in production.** The frozen test vectors elsewhere in this
/// crate are computed under these parameters; switching to production
/// parameters changes every derived secret.
pub fn test_fast_params() -> Params {
    Params::new(8, 1, 1, Some(32)).expect("Argon2id test-fast parameters are valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    // The byte literals below are part of the v1 wire format; failing
    // any of these means the constant has drifted.

    #[test]
    fn room_key_salt_is_32_bytes_with_label_prefix() {
        assert_eq!(ROOM_KEY_SALT.len(), 32);
        assert!(ROOM_KEY_SALT.starts_with(b"sunset-chat-v1-room"));
        // Trailing NUL pad.
        assert!(ROOM_KEY_SALT[19..].iter().all(|&b| b == 0));
    }

    #[test]
    fn fingerprint_domain_literal() {
        assert_eq!(FINGERPRINT_DOMAIN, b"sunset-chat-v1-fingerprint");
    }

    #[test]
    fn epoch_0_domain_literal() {
        assert_eq!(EPOCH_0_DOMAIN, b"sunset-chat-v1-epoch-0");
    }

    #[test]
    fn msg_key_domain_literal() {
        assert_eq!(MSG_KEY_DOMAIN, b"sunset-chat-v1-msg");
    }

    #[test]
    fn msg_aad_domain_literal() {
        assert_eq!(MSG_AAD_DOMAIN, b"sunset-chat-v1-msg-aad");
    }

    #[test]
    fn production_params_match_owasp_2023() {
        let p = production_params();
        assert_eq!(p.m_cost(), 19_456);
        assert_eq!(p.t_cost(), 2);
        assert_eq!(p.p_cost(), 1);
        assert_eq!(p.output_len(), Some(32));
    }

    #[test]
    fn test_fast_params_are_minimal() {
        let p = test_fast_params();
        assert_eq!(p.m_cost(), 8);
        assert_eq!(p.t_cost(), 1);
        assert_eq!(p.p_cost(), 1);
        assert_eq!(p.output_len(), Some(32));
    }
}
