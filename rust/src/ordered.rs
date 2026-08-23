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

use std::collections::hash_map::DefaultHasher;
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
        let hash = hash_of(&key);
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

fn hash_of<K: Hash>(key: &K) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
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
