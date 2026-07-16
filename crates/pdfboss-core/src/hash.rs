//! A fast, non-cryptographic hasher for the small keys PDF machinery hashes
//! in bulk: dictionary names (`/Type`, `/Font`, …), object ids `(num, gen)`,
//! and glyph indices. The standard library's default `HashMap` uses
//! SipHash-1-3, which is DoS-resistant but far slower than necessary for
//! this data — the keys are short, the maps are in-process, and the input
//! has already been parsed, so hash-flooding is not a concern here.
//!
//! The construction is the well-known "FxHash": fold each machine word of
//! the key into an accumulator with a rotate, an xor, and a multiply by a
//! fixed odd constant. It is not seeded (hashing is deterministic across
//! runs), which is exactly what we want for a trusted in-process map.

use std::hash::{BuildHasherDefault, Hasher};

/// A [`std::collections::HashMap`] using [`FxHasher`] instead of SipHash.
pub type FastMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;

/// A [`std::collections::HashSet`] using [`FxHasher`] instead of SipHash.
pub type FastSet<K> = std::collections::HashSet<K, BuildHasherDefault<FxHasher>>;

/// Odd multiplier giving good avalanche across the folded word.
const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
/// Left-rotation applied before folding each word in.
const ROTATE: u32 = 5;

/// The FxHash state: a single accumulator, folded one machine word at a
/// time. `Default` starts it at zero.
#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    /// Folds one 64-bit chunk of key material into the accumulator.
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(ROTATE) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        // Fold 8 bytes at a time, then a 4-byte block, then the final tail —
        // covering short name keys without a byte-at-a-time loop.
        while bytes.len() >= 8 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[..8]);
            self.add(u64::from_le_bytes(b));
            bytes = &bytes[8..];
        }
        if bytes.len() >= 4 {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes[..4]);
            self.add(u32::from_le_bytes(b) as u64);
            bytes = &bytes[4..];
        }
        for &b in bytes {
            self.add(u64::from(b));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    fn hash_of<T: Hash>(v: &T) -> u64 {
        let mut h = FxHasher::default();
        v.hash(&mut h);
        h.finish()
    }

    #[test]
    fn deterministic_and_key_sensitive() {
        // Same key hashes the same; different keys (very likely) differ.
        assert_eq!(hash_of(&"Font"), hash_of(&"Font"));
        assert_ne!(hash_of(&"Font"), hash_of(&"Type"));
        assert_ne!(hash_of(&(3u32, 0u16)), hash_of(&(3u32, 1u16)));
    }

    #[test]
    fn map_behaves_like_a_hashmap() {
        let mut m: FastMap<String, u32> = FastMap::default();
        m.insert("Kids".to_string(), 1);
        m.insert("Count".to_string(), 2);
        assert_eq!(m.get("Kids"), Some(&1));
        assert_eq!(m.get("Count"), Some(&2));
        assert_eq!(m.get("Missing"), None);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn set_dedups() {
        let mut s: FastSet<u32> = FastSet::default();
        assert!(s.insert(7));
        assert!(!s.insert(7));
        assert!(s.contains(&7));
    }
}
