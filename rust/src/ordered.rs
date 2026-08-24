//! A grouping map that remembers the order its keys first appeared.
//!
//! The oracle groups with Python dicts, which are insertion-ordered, and two
//! of the places it does so are observable: the small-file stage walks its
//! buckets in that order while charging them against `RETAIN_BUDGET`, so which
//! files fall back to the staged pipeline depends on it. A `HashMap` would
//! make that a coin toss.
//!
//! Keys are stored once. A large content key — a 64 KiB tier-B block — is what
//! the retention budget is counting, so holding a second copy of it for the
//! index would put the budget out by a factor of two.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

pub struct Ordered<K, V> {
    slots: Vec<(K, Vec<V>)>,
    index: HashMap<u64, Vec<usize>>,
}

impl<K: Hash + Eq, V> Ordered<K, V> {
    pub fn new() -> Self {
        Ordered {
            slots: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Add `value` under `key`. True when this key had not been seen before —
    /// which is when a caller charging a retention budget has to pay for it.
    pub fn push(&mut self, key: K, value: V) -> bool {
        self.push_hashed(hash_of(&key), key, value)
    }

    /// `push`, for a caller that already has `hash_of(&key)` in hand.
    ///
    /// The small-file stage hashes whole file contents, so the hash is a pass
    /// over every byte it read and the loop that does it is the one place in
    /// that stage still running on one thread. Handing the hash in lets the
    /// caller compute it wherever it read the bytes — the key still decides
    /// the grouping, and is still compared in full before a match is believed,
    /// so a wrong hash here could only cost a comparison, never a wrong group.
    pub fn push_hashed(&mut self, hash: u64, key: K, value: V) -> bool {
        let found = self.index.get(&hash).and_then(|slots| {
            slots
                .iter()
                .copied()
                .find(|&slot| self.slots[slot].0 == key)
        });
        if let Some(slot) = found {
            self.slots[slot].1.push(value);
            return false;
        }
        self.index.entry(hash).or_default().push(self.slots.len());
        self.slots.push((key, vec![value]));
        true
    }

    pub fn into_pairs(self) -> Vec<(K, Vec<V>)> {
        self.slots
    }

    pub fn into_values(self) -> Vec<Vec<V>> {
        self.slots.into_iter().map(|(_, values)| values).collect()
    }
}

pub fn hash_of<K: Hash>(key: &K) -> u64 {
    let mut hasher = Fast::default();
    key.hash(&mut hasher);
    hasher.finish()
}

/// A cheap hash for keys that are mostly file content.
///
/// The index this feeds is a hint and never an answer: `push` compares the
/// key itself before it believes a match, so a weaker hash costs an extra
/// comparison and cannot produce a wrong grouping. What it buys is real —
/// the small-file stage keys on whole file contents, so every byte the stage
/// reads is also a byte it hashes, and `DefaultHasher` is SipHash.
///
/// The digest goes on to be a `HashMap` key, where it is hashed again by that
/// map's own SipHash, so this does not have to spread its output well. It
/// only has to depend on every byte, which a multiply per eight of them does.
#[derive(Default)]
struct Fast(u64);

impl Fast {
    /// One round of the multiply-and-rotate mixer. The rotate is what stops a
    /// run of equal words cancelling itself out.
    fn eat(&mut self, word: u64) {
        const ODD: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(ODD);
    }
}

impl Hasher for Fast {
    fn write(&mut self, bytes: &[u8]) {
        let mut words = bytes.chunks_exact(8);
        for word in &mut words {
            self.eat(u64::from_le_bytes(word.try_into().expect("eight bytes")));
        }
        // The tail is padded rather than dropped, and the length is eaten
        // below, so two keys that differ only past the last whole word still
        // differ here.
        let rest = words.remainder();
        if !rest.is_empty() {
            let mut last = [0_u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.eat(u64::from_le_bytes(last));
        }
        self.eat(bytes.len() as u64);
    }

    fn write_u8(&mut self, value: u8) {
        self.eat(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.eat(value);
    }

    fn write_usize(&mut self, value: usize) {
        self.eat(value as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_come_back_in_the_order_they_first_appeared() {
        let mut map: Ordered<&str, u32> = Ordered::new();
        assert!(map.push("b", 1));
        assert!(map.push("a", 2));
        assert!(!map.push("b", 3));
        assert_eq!(map.into_pairs(), [("b", vec![1, 3]), ("a", vec![2])]);
    }

    #[test]
    fn equal_byte_keys_land_in_one_group() {
        let mut map: Ordered<Vec<u8>, u32> = Ordered::new();
        map.push(vec![1, 2, 3], 1);
        map.push(vec![1, 2, 3], 2);
        map.push(vec![1, 2, 4], 3);
        assert_eq!(map.into_values(), [vec![1, 2], vec![3]]);
    }
}

#[cfg(test)]
mod hashing {
    use super::*;

    #[test]
    fn every_byte_of_a_key_changes_its_hash() {
        let base = vec![7_u8; 4096];
        let plain = hash_of(&base);
        for at in [0, 1, 7, 8, 9, 2047, 4094, 4095] {
            let mut moved = base.clone();
            moved[at] ^= 1;
            assert_ne!(plain, hash_of(&moved), "byte {at} did not reach the hash");
        }
    }

    #[test]
    fn keys_that_differ_only_in_length_differ() {
        assert_ne!(hash_of(&vec![0_u8; 8]), hash_of(&vec![0_u8; 9]));
        assert_ne!(hash_of(&vec![3_u8; 100]), hash_of(&vec![3_u8; 101]));
    }

    /// The shape `prefixtrap` is made of: a long shared head, then a parting.
    #[test]
    fn a_long_shared_prefix_does_not_collapse_the_tail() {
        let mut left = vec![9_u8; 65_536];
        let mut right = left.clone();
        left.extend_from_slice(b"one");
        right.extend_from_slice(b"two");
        assert_ne!(hash_of(&left), hash_of(&right));
    }

    /// Distinct content must land in distinct slots, which is the property
    /// the small-file stage's grouping rests on.
    #[test]
    fn many_distinct_keys_do_not_pile_into_one_slot() {
        let mut map: Ordered<Vec<u8>, usize> = Ordered::new();
        for value in 0..5_000_u32 {
            let mut key = vec![0_u8; 256];
            key[..4].copy_from_slice(&value.to_le_bytes());
            assert!(
                map.push(key, value as usize),
                "{value} was seen as a repeat"
            );
        }
        assert_eq!(map.into_pairs().len(), 5_000);
    }
}
