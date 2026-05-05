//! Cert hash helpers — translate between hex and `wtransport::Sha256Digest`.

use wtransport::tls::Sha256Digest;

/// Lowercase 64-char hex digest of `digest`. Keeps the wire format
/// stable across the resolver / descriptor / address fragment.
pub fn sha256_digest_to_hex(digest: &Sha256Digest) -> String {
    let bytes: &[u8; 32] = digest.as_ref();
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push(hex_nibble(b >> 4));
        s.push(hex_nibble(b & 0x0f));
    }
    s
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '?',
    }
}

/// Parse a 64-char hex digest. Accepts upper or lower case; rejects any
/// other length.
pub fn parse_cert_hash_hex(s: &str) -> Result<Sha256Digest, String> {
    if s.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", s.len()));
    }
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        let hi = parse_nibble(s.as_bytes()[i * 2])
            .ok_or_else(|| format!("non-hex char at offset {}", i * 2))?;
        let lo = parse_nibble(s.as_bytes()[i * 2 + 1])
            .ok_or_else(|| format!("non-hex char at offset {}", i * 2 + 1))?;
        *b = (hi << 4) | lo;
    }
    Ok(Sha256Digest::new(bytes))
}

fn parse_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(10 + c - b'a'),
        b'A'..=b'F' => Some(10 + c - b'A'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hex() {
        let original = Sha256Digest::new([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba,
            0xdc, 0xfe, 0x00, 0xff, 0xaa, 0x55, 0x11, 0x22, 0x33, 0x44, 0x66, 0x77, 0x88, 0x99,
            0xcc, 0xdd, 0xee, 0xff,
        ]);
        let hex = sha256_digest_to_hex(&original);
        assert_eq!(hex.len(), 64);
        let parsed = parse_cert_hash_hex(&hex).unwrap();
        assert_eq!(parsed.as_ref(), original.as_ref());
    }

    #[test]
    fn rejects_short() {
        assert!(parse_cert_hash_hex("ab").is_err());
    }

    #[test]
    fn rejects_non_hex() {
        let mut s = "a".repeat(64);
        s.replace_range(10..11, "z");
        assert!(parse_cert_hash_hex(&s).is_err());
    }

    #[test]
    fn case_insensitive() {
        let lower = "abcdef0123456789".repeat(4);
        let upper = lower.to_uppercase();
        let a = parse_cert_hash_hex(&lower).unwrap();
        let b = parse_cert_hash_hex(&upper).unwrap();
        assert_eq!(a.as_ref(), b.as_ref());
    }
}
