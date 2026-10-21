//! Small shared utilities.

use std::hash::{BuildHasherDefault, Hasher};

/// The Fx hash (from rustc): a fast, non-cryptographic hasher for the engine's
/// internal maps (font selection, glyph caches, style sharing, rule buckets), whose
/// keys are not attacker-controlled in a way that matters for a renderer.
#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.add(u64::from_le_bytes(*c));
        }
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(buf));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
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

/// A `HashMap` using [`FxHasher`].
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_works_and_hash_differs() {
        let mut m: FxHashMap<String, u32> = FxHashMap::default();
        m.insert("a".into(), 1);
        m.insert("bb".into(), 2);
        assert_eq!(m.get("bb"), Some(&2));
        let h = |s: &str| {
            let mut x = FxHasher::default();
            x.write(s.as_bytes());
            x.finish()
        };
        assert_ne!(h("abc"), h("abd"));
    }
}
