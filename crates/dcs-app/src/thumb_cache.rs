//! Decoded-thumbnail caching for one tier. A [`ThumbCache`] pairs the resident
//! pixels (LRU-bounded by bytes) with the set of decodes currently in flight, so
//! the two can never be hand-synced out of step: a finished decode leaves
//! `inflight` exactly as it enters `resident`. The session holds one per tier
//! (base / hi-res / gallery).

use std::collections::HashSet;

use dcs_domain::photo::PhotoId;
use dcs_domain::thumb::ThumbImage;

use crate::util::LruMap;

/// A resident thumbnail. `version` bumps on every change so the UI knows when to
/// re-upload its texture (base → hi-res on zoom, or back on zoom-out).
struct CachedThumb {
    image: ThumbImage,
    version: u64,
}

/// Borrowed view of a resident thumbnail handed to the UI.
#[derive(Clone, Copy)]
pub struct ThumbView<'a> {
    pub image: &'a ThumbImage,
    pub version: u64,
}

/// One decode tier's state: the resident pixels paired with the in-flight set.
pub(crate) struct ThumbCache {
    resident: LruMap<PhotoId, CachedThumb>,
    inflight: HashSet<PhotoId>,
}

impl ThumbCache {
    pub(crate) fn new(budget: u64) -> Self {
        ThumbCache {
            resident: LruMap::new(budget),
            inflight: HashSet::new(),
        }
    }

    /// Drop all resident pixels and pending markers (zoom-out, folder change).
    pub(crate) fn reset(&mut self, budget: u64) {
        self.resident = LruMap::new(budget);
        self.inflight.clear();
    }

    pub(crate) fn is_inflight(&self, id: PhotoId) -> bool {
        self.inflight.contains(&id)
    }

    /// True when a decode for `id` is neither resident nor already in flight — so
    /// one should be started. Touches the entry, keeping a present thumbnail
    /// recently used.
    pub(crate) fn idle(&mut self, id: PhotoId) -> bool {
        !self.inflight.contains(&id) && self.resident.get(&id).is_none()
    }

    /// Mark a decode started.
    pub(crate) fn start(&mut self, id: PhotoId) {
        self.inflight.insert(id);
    }

    /// The resident thumbnail for `id`, marking it recently used.
    pub(crate) fn view(&mut self, id: PhotoId) -> Option<ThumbView<'_>> {
        let cached = self.resident.get(&id)?;
        Some(ThumbView {
            image: &cached.image,
            version: cached.version,
        })
    }

    /// Store a finished decode under `version`, retiring its in-flight marker.
    /// Returns the ids evicted to stay within budget.
    pub(crate) fn store(&mut self, id: PhotoId, image: ThumbImage, version: u64) -> Vec<PhotoId> {
        self.inflight.remove(&id);
        let bytes = image.rgba.len() as u64;
        self.resident
            .insert(id, CachedThumb { image, version }, bytes)
    }

    /// Retire an in-flight marker without storing pixels (a failed decode).
    pub(crate) fn fail(&mut self, id: PhotoId) {
        self.inflight.remove(&id);
    }

    pub(crate) fn pending(&self) -> bool {
        !self.inflight.is_empty()
    }

    pub(crate) fn inflight_len(&self) -> usize {
        self.inflight.len()
    }

    pub(crate) fn len(&self) -> usize {
        self.resident.len()
    }

    pub(crate) fn weight(&self) -> u64 {
        self.resident.weight()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(bytes: usize) -> ThumbImage {
        ThumbImage {
            width: 1,
            height: bytes as u32,
            rgba: vec![0u8; bytes],
        }
    }

    #[test]
    fn idle_until_inflight_or_resident() {
        let mut c = ThumbCache::new(1_000);
        let id = PhotoId(1);
        assert!(c.idle(id), "fresh id needs a decode");

        c.start(id);
        assert!(!c.idle(id), "in flight → not idle");
        assert!(c.is_inflight(id));
        assert!(c.pending());

        let evicted = c.store(id, img(100), 7);
        assert!(evicted.is_empty());
        assert!(!c.is_inflight(id), "store retires the in-flight marker");
        assert!(!c.pending());
        assert!(!c.idle(id), "resident → not idle");
        assert_eq!(c.view(id).unwrap().version, 7);
    }

    #[test]
    fn fail_retires_inflight_without_storing() {
        let mut c = ThumbCache::new(1_000);
        let id = PhotoId(2);
        c.start(id);
        c.fail(id);
        assert!(!c.is_inflight(id));
        assert!(c.view(id).is_none(), "failed decode stores no pixels");
        assert!(c.idle(id), "a failed decode can be retried");
    }

    #[test]
    fn store_evicts_over_budget_and_reports_ids() {
        // Budget fits two 100-byte entries; a third evicts the least-recent.
        let mut c = ThumbCache::new(250);
        c.store(PhotoId(1), img(100), 1);
        c.store(PhotoId(2), img(100), 2);
        let _ = c.view(PhotoId(1)); // touch 1 so 2 is the victim
        let evicted = c.store(PhotoId(3), img(100), 3);
        assert_eq!(evicted, vec![PhotoId(2)]);
        assert!(c.view(PhotoId(2)).is_none());
        assert!(c.view(PhotoId(1)).is_some());
        assert!(c.view(PhotoId(3)).is_some());
    }

    #[test]
    fn reset_clears_pixels_and_inflight() {
        let mut c = ThumbCache::new(1_000);
        c.start(PhotoId(1));
        c.store(PhotoId(2), img(100), 1);
        c.reset(1_000);
        assert!(!c.pending());
        assert_eq!(c.len(), 0);
        assert_eq!(c.weight(), 0);
        assert!(c.view(PhotoId(2)).is_none());
    }
}
