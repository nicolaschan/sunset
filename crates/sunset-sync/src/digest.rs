//! Bloom filter for `DigestExchange` and the digest-round state machine.

use bytes::Bytes;

/// A simple bloom filter backed by a fixed-size byte vector.
///
/// `num_bits` MUST be a multiple of 8 (the byte vector's length is
/// `num_bits / 8`). `num_hashes` controls the false-positive rate. v1 uses
/// fixed defaults from `SyncConfig` (4096 bits, 4 hashes).
#[derive(Clone, Debug)]
pub struct BloomFilter {
    bits: Vec<u8>,
    num_bits: usize,
    num_hashes: u32,
}

impl BloomFilter {
    pub fn new(num_bits: usize, num_hashes: u32) -> Self {
        debug_assert!(
            num_bits % 8 == 0 && num_bits > 0,
            "num_bits must be a positive multiple of 8"
        );
        Self {
            bits: vec![0u8; num_bits / 8],
            num_bits,
            num_hashes,
        }
    }

    pub fn from_bytes(bytes: Bytes, num_hashes: u32) -> Self {
        let num_bits = bytes.len() * 8;
        Self {
            bits: bytes.to_vec(),
            num_bits,
            num_hashes,
        }
    }

    pub fn to_bytes(&self) -> Bytes {
        Bytes::copy_from_slice(&self.bits)
    }

    pub fn num_bits(&self) -> usize {
        self.num_bits
    }
    pub fn num_hashes(&self) -> u32 {
        self.num_hashes
    }

    pub fn insert(&mut self, item: &[u8]) {
        for h in 0..self.num_hashes {
            let bit = self.bit_index(item, h);
            let (byte, mask) = (bit / 8, 1u8 << (bit % 8));
            self.bits[byte] |= mask;
        }
    }

    pub fn contains(&self, item: &[u8]) -> bool {
        for h in 0..self.num_hashes {
            let bit = self.bit_index(item, h);
            let (byte, mask) = (bit / 8, 1u8 << (bit % 8));
            if self.bits[byte] & mask == 0 {
                return false;
            }
        }
        true
    }

    /// Bit index for the `h`th hash of `item`. Uses blake3 with the hash
    /// index as a 4-byte salt prefix.
    fn bit_index(&self, item: &[u8], h: u32) -> usize {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&h.to_le_bytes());
        hasher.update(item);
        let digest = hasher.finalize();
        let bytes = digest.as_bytes();
        let mut idx = [0u8; 8];
        idx.copy_from_slice(&bytes[..8]);
        (u64::from_le_bytes(idx) as usize) % self.num_bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_contains() {
        let mut b = BloomFilter::new(4096, 4);
        b.insert(b"alice");
        assert!(b.contains(b"alice"));
    }

    #[test]
    fn contains_false_for_unset() {
        let b = BloomFilter::new(4096, 4);
        assert!(!b.contains(b"alice"));
    }

    #[test]
    fn bytes_roundtrip() {
        let mut b = BloomFilter::new(4096, 4);
        b.insert(b"alice");
        b.insert(b"bob");
        let bytes = b.to_bytes();
        let b2 = BloomFilter::from_bytes(bytes, 4);
        assert!(b2.contains(b"alice"));
        assert!(b2.contains(b"bob"));
        assert!(!b2.contains(b"carol"));
    }

    #[test]
    fn empty_filter_contains_nothing() {
        let b = BloomFilter::new(4096, 4);
        for item in [b"a".as_ref(), b"b".as_ref(), b"c".as_ref()] {
            assert!(!b.contains(item));
        }
    }
}
