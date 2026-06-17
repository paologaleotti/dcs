//! Small internal utilities.

use std::collections::HashMap;
use std::hash::Hash;

use dcs_domain::photo::PhotoId;

/// Which resident cache a decode result belongs to. Packed into the decode key
/// so the session routes each result to the right tier on arrival.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeTier {
    /// Cheap 256 px grid thumbnail.
    Base,
    /// Sharp viewport decode while the grid is zoomed in.
    Hires,
    /// Large fit/1:1 decode for the gallery view (§2.13).
    Gallery,
}

impl DecodeTier {
    fn bits(self) -> u64 {
        match self {
            DecodeTier::Base => 0,
            DecodeTier::Hires => 1,
            DecodeTier::Gallery => 2,
        }
    }

    fn from_bits(bits: u64) -> DecodeTier {
        match bits & 0b11 {
            0 => DecodeTier::Base,
            1 => DecodeTier::Hires,
            _ => DecodeTier::Gallery,
        }
    }
}

/// Pack a folder epoch, a `PhotoId`, and the decode tier into one key. The epoch
/// lets the session discard thumbnails decoded for a folder that has since been
/// closed (ids restart at 0 per folder); the tier bits route the result to the
/// right cache.
pub(crate) fn encode_key(epoch: u64, id: PhotoId, tier: DecodeTier) -> u64 {
    (epoch << 34) | ((id.0 as u64) << 2) | tier.bits()
}

pub(crate) fn decode_key(key: u64) -> (u64, PhotoId, DecodeTier) {
    (
        key >> 34,
        PhotoId(((key >> 2) & 0xFFFF_FFFF) as u32),
        DecodeTier::from_bits(key),
    )
}

/// Least-recently-used map bounded by a total weight budget (bytes, for the
/// thumbnail caches). Entries carry a per-item weight; inserting past the budget
/// evicts least-recently-used entries until it fits.
///
/// Recency is an intrusive doubly-linked list over a slot slab: `head` is the
/// most-recently-used, `tail` the least. `get`/`insert` splice a node to the
/// head and eviction pops the tail — all O(1), so a full cache churning on a
/// large folder never pays an O(n) victim scan.
pub struct LruMap<K, V> {
    budget: u64,
    used: u64,
    index: HashMap<K, usize>,
    slots: Vec<Slot<K, V>>,
    free: Vec<usize>,
    head: Option<usize>,
    tail: Option<usize>,
}

struct Slot<K, V> {
    key: K,
    /// `Some` while the slot is live; set to `None` the instant the slot is
    /// recycled so the (potentially large) value's memory is freed at eviction
    /// time, not deferred until a later insert reuses the slot.
    value: Option<V>,
    weight: u64,
    prev: Option<usize>,
    next: Option<usize>,
}

impl<K: Eq + Hash + Copy, V> LruMap<K, V> {
    pub fn new(budget: u64) -> Self {
        LruMap {
            budget: budget.max(1),
            used: 0,
            index: HashMap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            head: None,
            tail: None,
        }
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Total resident weight (bytes).
    pub fn weight(&self) -> u64 {
        self.used
    }

    /// The value for `key`, promoted to most-recently-used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let slot = *self.index.get(key)?;
        self.detach(slot);
        self.push_front(slot);
        self.slots[slot].value.as_ref()
    }

    /// Insert with the given weight, evicting least-recently-used entries until
    /// the total fits the budget. Returns every key evicted.
    pub fn insert(&mut self, key: K, value: V, weight: u64) -> Vec<K> {
        if let Some(&slot) = self.index.get(&key) {
            // Replace in place: swap value/weight, promote, then trim to budget.
            self.used -= self.slots[slot].weight;
            self.slots[slot].value = Some(value);
            self.slots[slot].weight = weight;
            self.used += weight;
            self.detach(slot);
            self.push_front(slot);
        } else {
            let slot = self.alloc(key, value, weight);
            self.index.insert(key, slot);
            self.push_front(slot);
            self.used += weight;
        }
        self.evict_to_budget()
    }

    /// Evict from the tail until within budget (keeping at least one entry).
    /// Returns the evicted keys.
    fn evict_to_budget(&mut self) -> Vec<K> {
        let mut evicted = Vec::new();
        while self.used > self.budget && self.index.len() > 1 {
            let Some(tail) = self.tail else { break };
            self.detach(tail);
            let key = self.slots[tail].key;
            self.index.remove(&key);
            self.used -= self.slots[tail].weight;
            self.recycle(tail);
            evicted.push(key);
        }
        evicted
    }

    /// Claim a slot (reusing a freed one when possible) holding the entry.
    fn alloc(&mut self, key: K, value: V, weight: u64) -> usize {
        let slot = Slot {
            key,
            value: Some(value),
            weight,
            prev: None,
            next: None,
        };
        if let Some(idx) = self.free.pop() {
            self.slots[idx] = slot;
            idx
        } else {
            self.slots.push(slot);
            self.slots.len() - 1
        }
    }

    /// Return a detached slot to the free list, dropping its value now so its
    /// memory is reclaimed at eviction rather than lingering until reuse.
    fn recycle(&mut self, slot: usize) {
        self.slots[slot].value = None;
        self.free.push(slot);
    }

    /// Unlink a slot from the recency list (its neighbours close over it).
    fn detach(&mut self, slot: usize) {
        let (prev, next) = (self.slots[slot].prev, self.slots[slot].next);
        match prev {
            Some(p) => self.slots[p].next = next,
            None => self.head = next,
        }
        match next {
            Some(n) => self.slots[n].prev = prev,
            None => self.tail = prev,
        }
        self.slots[slot].prev = None;
        self.slots[slot].next = None;
    }

    /// Splice a detached slot in as the most-recently-used head.
    fn push_front(&mut self, slot: usize) {
        self.slots[slot].prev = None;
        self.slots[slot].next = self.head;
        if let Some(h) = self.head {
            self.slots[h].prev = Some(slot);
        }
        self.head = Some(slot);
        if self.tail.is_none() {
            self.tail = Some(slot);
        }
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
    fn access_promotes_so_eviction_follows_true_recency() {
        // Budget for 3 unit entries; insert 1,2,3 then touch in a custom order.
        let mut lru: LruMap<u32, u32> = LruMap::new(3);
        for k in 1..=3 {
            lru.insert(k, k * 10, 1);
        }
        // Touch 1 then 2; recency (LRU→MRU) is now 3, 1, 2.
        assert_eq!(lru.get(&1), Some(&10));
        assert_eq!(lru.get(&2), Some(&20));
        // Inserting 4 evicts the least-recent (3), then 5 evicts the next (1).
        assert_eq!(lru.insert(4, 40, 1), vec![3]);
        assert_eq!(lru.insert(5, 50, 1), vec![1]);
        assert_eq!(lru.get(&3), None);
        assert_eq!(lru.get(&1), None);
        // 2, 4, 5 remain.
        assert!(lru.get(&2).is_some() && lru.get(&4).is_some() && lru.get(&5).is_some());
        assert_eq!(lru.len(), 3);
    }

    #[test]
    fn eviction_frees_the_value_immediately_not_on_reuse() {
        use std::rc::Rc;
        let mut lru: LruMap<u32, Rc<()>> = LruMap::new(2);
        let one = Rc::new(());
        lru.insert(1, one.clone(), 1);
        lru.insert(2, Rc::new(()), 1);
        assert_eq!(Rc::strong_count(&one), 2, "the cache holds value 1");
        // Inserting 3 evicts the least-recently-used (1). Its value must drop at
        // eviction — not linger in a recycled slot until a later insert reuses it.
        lru.insert(3, Rc::new(()), 1);
        assert_eq!(lru.get(&1), None);
        assert_eq!(
            Rc::strong_count(&one),
            1,
            "evicted value freed at eviction, not deferred to slot reuse"
        );
    }

    #[test]
    fn churn_keeps_weight_and_len_consistent_with_slot_reuse() {
        // Far more inserts than the budget holds, forcing constant eviction and
        // slot recycling; the slab must never leak weight or grow unbounded.
        let mut lru: LruMap<u32, u32> = LruMap::new(5);
        for k in 0..1000 {
            lru.insert(k, k, 1);
        }
        assert_eq!(lru.len(), 5);
        assert_eq!(lru.weight(), 5);
        // Only the last five keys survive.
        for k in 995..1000 {
            assert!(lru.get(&k).is_some(), "recent key {k} resident");
        }
        for k in 0..995 {
            assert!(lru.get(&k).is_none(), "old key {k} evicted");
        }
    }

    #[test]
    fn decode_key_round_trips_every_tier() {
        for tier in [DecodeTier::Base, DecodeTier::Hires, DecodeTier::Gallery] {
            assert_eq!(
                decode_key(encode_key(7, PhotoId(123), tier)),
                (7, PhotoId(123), tier)
            );
        }
    }

    #[test]
    fn keys_differ_by_epoch_and_tier_for_same_id() {
        assert_ne!(
            encode_key(1, PhotoId(5), DecodeTier::Base),
            encode_key(2, PhotoId(5), DecodeTier::Base)
        );
        assert_ne!(
            encode_key(1, PhotoId(5), DecodeTier::Base),
            encode_key(1, PhotoId(5), DecodeTier::Hires)
        );
        assert_ne!(
            encode_key(1, PhotoId(5), DecodeTier::Hires),
            encode_key(1, PhotoId(5), DecodeTier::Gallery)
        );
    }
}
