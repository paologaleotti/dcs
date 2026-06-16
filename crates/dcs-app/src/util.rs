//! Small internal utilities.

use std::collections::HashMap;
use std::hash::Hash;

use dcs_domain::photo::PhotoId;

/// Pack a folder epoch, a `PhotoId`, and the base/hi-res tier into one decode
/// key. The epoch lets the session discard thumbnails decoded for a folder that
/// has since been closed (ids restart at 0 per folder); the tier bit routes the
/// result to the right cache.
pub(crate) fn encode_key(epoch: u64, id: PhotoId, hires: bool) -> u64 {
    (epoch << 33) | ((id.0 as u64) << 1) | hires as u64
}

pub(crate) fn decode_key(key: u64) -> (u64, PhotoId, bool) {
    (
        key >> 33,
        PhotoId(((key >> 1) & 0xFFFF_FFFF) as u32),
        key & 1 == 1,
    )
}

/// Least-recently-used map bounded by a total weight budget (bytes, for the
/// thumbnail caches). Entries carry a per-item weight; inserting past the
/// budget evicts the least-recently-used entries until it fits. Recency is a
/// monotonic access counter; insertion and access both count as a use.
pub struct LruMap<K, V> {
    budget: u64,
    used: u64,
    tick: u64,
    map: HashMap<K, Entry<V>>,
}

struct Entry<V> {
    tick: u64,
    weight: u64,
    value: V,
}

impl<K: Eq + Hash + Copy, V> LruMap<K, V> {
    pub fn new(budget: u64) -> Self {
        LruMap {
            budget: budget.max(1),
            used: 0,
            tick: 0,
            map: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Total resident weight (bytes).
    pub fn weight(&self) -> u64 {
        self.used
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.tick = tick;
        Some(&entry.value)
    }

    /// Insert with the given weight, evicting least-recently-used entries until
    /// the total fits the budget. Returns every key evicted.
    pub fn insert(&mut self, key: K, value: V, weight: u64) -> Vec<K> {
        self.tick += 1;
        let entry = Entry {
            tick: self.tick,
            weight,
            value,
        };
        if let Some(old) = self.map.insert(key, entry) {
            self.used -= old.weight;
        }
        self.used += weight;

        let mut evicted = Vec::new();
        // FIXME(perf): O(n) scan to find the LRU victim, per eviction. Only runs
        // when a cache exceeds its budget (never for folders within it), so it's
        // free at current sizes. For 10k+ folders, replace with an intrusive
        // LRU list or a min-heap on `tick` for O(1)/O(log n) eviction.
        while self.used > self.budget && self.map.len() > 1 {
            let Some(victim) = self
                .map
                .iter()
                .min_by_key(|(_, entry)| entry.tick)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(removed) = self.map.remove(&victim) {
                self.used -= removed.weight;
                evicted.push(victim);
            }
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used_over_budget() {
        // Budget of 2 unit-weight entries.
        let mut lru: LruMap<u32, u32> = LruMap::new(2);
        assert!(lru.insert(1, 10, 1).is_empty());
        assert!(lru.insert(2, 20, 1).is_empty());
        // Touch 1 so 2 becomes least-recently-used.
        assert_eq!(lru.get(&1), Some(&10));
        let evicted = lru.insert(3, 30, 1);
        assert_eq!(evicted, vec![2]);
        assert_eq!(lru.get(&2), None);
        assert_eq!(lru.get(&3), Some(&30));
        assert_eq!(lru.get(&1), Some(&10));
        assert_eq!(lru.len(), 2);
    }

    #[test]
    fn heavy_insert_evicts_several_to_fit_budget() {
        let mut lru: LruMap<u32, u32> = LruMap::new(10);
        lru.insert(1, 10, 4);
        lru.insert(2, 20, 4);
        assert_eq!(lru.weight(), 8);
        // A weight-6 entry must push the total to 14 → evict until ≤ 10.
        let evicted = lru.insert(3, 30, 6);
        assert!(evicted.contains(&1));
        assert!(lru.weight() <= 10);
        assert_eq!(lru.get(&3), Some(&30));
    }

    #[test]
    fn reinserting_a_key_replaces_value_and_weight() {
        let mut lru: LruMap<u32, u32> = LruMap::new(100);
        lru.insert(1, 10, 8);
        let evicted = lru.insert(1, 99, 3);
        assert!(evicted.is_empty());
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.weight(), 3, "old weight is replaced, not added");
        assert_eq!(lru.get(&1), Some(&99));
    }

    #[test]
    fn budget_is_at_least_one() {
        let mut lru: LruMap<u32, u32> = LruMap::new(0);
        lru.insert(1, 10, 1);
        lru.insert(2, 20, 1);
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn decode_key_round_trips_encode_key() {
        assert_eq!(
            decode_key(encode_key(7, PhotoId(123), false)),
            (7, PhotoId(123), false)
        );
        assert_eq!(
            decode_key(encode_key(7, PhotoId(123), true)),
            (7, PhotoId(123), true)
        );
    }

    #[test]
    fn keys_differ_by_epoch_and_tier_for_same_id() {
        assert_ne!(
            encode_key(1, PhotoId(5), false),
            encode_key(2, PhotoId(5), false)
        );
        assert_ne!(
            encode_key(1, PhotoId(5), false),
            encode_key(1, PhotoId(5), true)
        );
    }
}
