//! Owned tag store: tag definitions plus the many-to-many photo↔tag
//! assignments, with a monotonic id allocator. Tags are the only persisted
//! user-created structure. Like [`crate::cull::Cull`] this holds
//! state and the primitives to apply/reverse a `TagDelta`; the undo timeline
//! lives in [`crate::history::History`].
//!
//! Two indexes are kept in step so both hot reads are O(1)-ish: `photo_tags`
//! (a cell's strips) and `tag_photos` (a tag band's members + unique count).

use std::collections::{BTreeSet, HashMap, HashSet};

use dcs_domain::command::TagDelta;
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::{Color, Tag, TagId, normalize_name};

/// Most tag-color strips shown on a cell's bottom edge; beyond this the edge
/// would slice into unreadable slivers, so extra tags are summarized elsewhere.
pub const MAX_STRIP: usize = 6;

/// The owned tag store. Empty tags keep their definition (only the *render* is
/// suppressed); a definition leaves only via delete or merge.
#[derive(Default)]
pub struct TagStore {
    defs: HashMap<TagId, Tag>,
    photo_tags: HashMap<PhotoId, BTreeSet<TagId>>,
    tag_photos: HashMap<TagId, HashSet<PhotoId>>,
    next_id: u32,
}

impl TagStore {
    pub fn new() -> Self {
        TagStore::default()
    }

    /// Rebuild from persisted state on reopen: the tag defs, the per-photo
    /// assignments, and the id counter (max assigned + 1).
    pub fn from_state(
        defs: impl IntoIterator<Item = Tag>,
        assignments: impl IntoIterator<Item = (PhotoId, Vec<TagId>)>,
        next_id: u32,
    ) -> Self {
        let mut store = TagStore {
            defs: defs.into_iter().map(|t| (t.id, t)).collect(),
            photo_tags: HashMap::new(),
            tag_photos: HashMap::new(),
            next_id,
        };
        // Reconcile the counter against the defs so a stale or hand-edited
        // `next_tag_id` (smaller than an existing def id) can't allocate a TagId
        // that overwrites a live definition. Mirrors `insert_def`'s self-heal.
        let max_def = store.defs.keys().map(|t| t.0 + 1).max().unwrap_or(0);
        store.next_id = store.next_id.max(max_def);
        for (photo, tags) in assignments {
            for tag in tags {
                // Only honour assignments whose tag still has a definition.
                if store.defs.contains_key(&tag) {
                    store.link(tag, photo);
                }
            }
        }
        store
    }

    /// The id counter to persist (max assigned id + 1), so fresh tags never
    /// collide with reclaimed ones after reopen.
    pub fn next_id(&self) -> u32 {
        self.next_id
    }

    /// All tag definitions, ordered by id (stable, creation order).
    pub fn defs(&self) -> Vec<Tag> {
        let mut tags: Vec<Tag> = self.defs.values().cloned().collect();
        tags.sort_by_key(|t| t.id);
        tags
    }

    /// The tag definition for an id, if it exists.
    pub fn def(&self, id: TagId) -> Option<&Tag> {
        self.defs.get(&id)
    }

    /// The tags on a photo, ordered by id. Empty when untagged.
    pub fn tags_of(&self, photo: PhotoId) -> Vec<TagId> {
        self.photo_tags
            .get(&photo)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    /// The per-photo tag index, borrowed for tag-band derivation — lets the pure
    /// grouping read membership without the caller materializing a copy.
    pub fn photo_tag_index(&self) -> &HashMap<PhotoId, BTreeSet<TagId>> {
        &self.photo_tags
    }

    /// A borrowed `id → name` map for tag-band titles — avoids cloning the tag
    /// defs on every regroup.
    pub fn name_map(&self) -> HashMap<TagId, &str> {
        self.defs
            .values()
            .map(|t| (t.id, t.name.as_str()))
            .collect()
    }

    /// Whether a photo carries a tag.
    pub fn is_assigned(&self, tag: TagId, photo: PhotoId) -> bool {
        self.tag_photos
            .get(&tag)
            .is_some_and(|s| s.contains(&photo))
    }

    /// Unique photos carrying a tag — the band's count (projections counted
    /// once, since a photo appears in the set once).
    pub fn photo_count(&self, tag: TagId) -> usize {
        self.tag_photos.get(&tag).map_or(0, |s| s.len())
    }

    /// The strip colors for a photo (its lowest-id tags), up to [`MAX_STRIP`].
    /// Allocation-free for the grid's per-cell hot path; tags without a
    /// definition are skipped. The grid divides the cell's bottom edge evenly
    /// among the present colors.
    pub fn strip(&self, photo: PhotoId) -> [Option<Color>; MAX_STRIP] {
        let mut colors = [None; MAX_STRIP];
        let Some(tags) = self.photo_tags.get(&photo) else {
            return colors;
        };
        for (slot, def) in tags
            .iter()
            .filter_map(|t| self.defs.get(t))
            .take(MAX_STRIP)
            .enumerate()
        {
            colors[slot] = Some(def.color);
        }
        colors
    }

    /// The tag whose name matches `name` (trimmed, case-insensitive), if any.
    pub fn id_by_name(&self, name: &str) -> Option<TagId> {
        let target = normalize_name(name);
        self.defs
            .values()
            .find(|t| normalize_name(&t.name) == target)
            .map(|t| t.id)
    }

    /// Like [`Self::id_by_name`] but excluding `except` — drives the
    /// merge-via-rename rule (a rename onto another tag's name merges).
    pub fn find_by_name(&self, name: &str, except: TagId) -> Option<TagId> {
        self.id_by_name(name).filter(|&id| id != except)
    }

    /// Forget photos entirely: drop them from every tag. Maintenance for missing
    /// pruning; scrubbing the undo timeline is the history's job.
    pub fn forget(&mut self, ids: &HashSet<PhotoId>) {
        for &id in ids {
            if let Some(tags) = self.photo_tags.remove(&id) {
                for t in tags {
                    if let Some(set) = self.tag_photos.get_mut(&t) {
                        set.remove(&id);
                    }
                }
            }
        }
    }

    /// Compute (and apply) the deltas for assigning `tag` to `ids`: one
    /// `Assigned` per photo that didn't already carry it. Deduped; empty when
    /// nothing moved or the tag is undefined.
    pub fn apply_assign(&mut self, tag: TagId, ids: &[PhotoId]) -> Vec<TagDelta> {
        if !self.defs.contains_key(&tag) {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        let mut deltas = Vec::new();
        for &photo in ids {
            if !seen.insert(photo) || self.is_assigned(tag, photo) {
                continue;
            }
            self.link(tag, photo);
            deltas.push(TagDelta::Assigned(tag, photo));
        }
        deltas
    }

    /// Deltas for removing `tag` from `ids`: one `Unassigned` per photo that
    /// carried it.
    pub fn apply_unassign(&mut self, tag: TagId, ids: &[PhotoId]) -> Vec<TagDelta> {
        let mut seen = HashSet::new();
        let mut deltas = Vec::new();
        for &photo in ids {
            if !seen.insert(photo) || !self.is_assigned(tag, photo) {
                continue;
            }
            self.unlink(tag, photo);
            deltas.push(TagDelta::Unassigned(tag, photo));
        }
        deltas
    }

    /// Create a new tag, allocating its id. Returns the `Created` delta.
    pub fn apply_create(&mut self, name: String, color: Color) -> Vec<TagDelta> {
        let id = TagId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let tag = Tag { id, name, color };
        self.insert_def(tag.clone());
        vec![TagDelta::Created(tag)]
    }

    /// Rename a tag. Renaming onto an existing name merges into it instead;
    /// a no-op (same normalized name, no other match) records nothing. Empty
    /// when the tag is undefined.
    pub fn apply_rename(&mut self, id: TagId, name: String) -> Vec<TagDelta> {
        let Some(current) = self.defs.get(&id) else {
            return Vec::new();
        };
        if let Some(into) = self.find_by_name(&name, id) {
            return self.apply_merge(into, id);
        }
        let before = current.name.clone();
        if before == name {
            return Vec::new();
        }
        if let Some(def) = self.defs.get_mut(&id) {
            def.name = name.clone();
        }
        vec![TagDelta::Renamed {
            id,
            before,
            after: name,
        }]
    }

    /// Recolor a tag, returning the reversible delta. Empty when the tag is
    /// undefined or already that color.
    pub fn apply_recolor(&mut self, id: TagId, color: Color) -> Vec<TagDelta> {
        let Some(def) = self.defs.get_mut(&id) else {
            return Vec::new();
        };
        if def.color == color {
            return Vec::new();
        }
        let before = def.color;
        def.color = color;
        vec![TagDelta::Recolored {
            id,
            before,
            after: color,
        }]
    }

    /// Merge `from` into `into`: every `from` photo gains `into` (if missing)
    /// and loses `from`, then `from`'s definition is removed. The delta order
    /// (assigns + unassigns, then the def removal) inverts cleanly in reverse.
    pub fn apply_merge(&mut self, into: TagId, from: TagId) -> Vec<TagDelta> {
        if into == from || !self.defs.contains_key(&into) {
            return Vec::new();
        }
        let Some(from_def) = self.defs.get(&from).cloned() else {
            return Vec::new();
        };
        let photos: Vec<PhotoId> = self
            .tag_photos
            .get(&from)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let mut deltas = Vec::new();
        for &photo in &photos {
            if !self.is_assigned(into, photo) {
                self.link(into, photo);
                deltas.push(TagDelta::Assigned(into, photo));
            }
            self.unlink(from, photo);
            deltas.push(TagDelta::Unassigned(from, photo));
        }
        self.remove_def(from);
        deltas.push(TagDelta::Removed(from_def));
        deltas
    }

    /// Delete a tag and all its assignments. Unassigns precede the def removal
    /// so undo re-creates the def before re-linking photos.
    pub fn apply_delete(&mut self, id: TagId) -> Vec<TagDelta> {
        let Some(def) = self.defs.get(&id).cloned() else {
            return Vec::new();
        };
        let photos: Vec<PhotoId> = self
            .tag_photos
            .get(&id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let mut deltas = Vec::new();
        for photo in photos {
            self.unlink(id, photo);
            deltas.push(TagDelta::Unassigned(id, photo));
        }
        self.remove_def(id);
        deltas.push(TagDelta::Removed(def));
        deltas
    }

    /// Re-apply a recorded delta set in order (redo).
    pub fn apply(&mut self, deltas: &[TagDelta]) {
        for d in deltas {
            self.apply_one(d);
        }
    }

    /// Reverse a recorded delta set (undo): invert each, in reverse order.
    pub fn revert(&mut self, deltas: &[TagDelta]) {
        for d in deltas.iter().rev() {
            self.apply_one(&d.invert());
        }
    }

    fn apply_one(&mut self, delta: &TagDelta) {
        match delta {
            TagDelta::Assigned(t, p) => self.link(*t, *p),
            TagDelta::Unassigned(t, p) => self.unlink(*t, *p),
            TagDelta::Created(tag) => self.insert_def(tag.clone()),
            TagDelta::Removed(tag) => self.remove_def(tag.id),
            TagDelta::Renamed { id, after, .. } => {
                if let Some(def) = self.defs.get_mut(id) {
                    def.name = after.clone();
                }
            }
            TagDelta::Recolored { id, after, .. } => {
                if let Some(def) = self.defs.get_mut(id) {
                    def.color = *after;
                }
            }
        }
    }

    fn insert_def(&mut self, tag: Tag) {
        // Keep the allocator ahead of any id re-created by undo/replay.
        self.next_id = self.next_id.max(tag.id.0 + 1);
        self.defs.insert(tag.id, tag);
    }

    fn remove_def(&mut self, id: TagId) {
        self.defs.remove(&id);
        if let Some(photos) = self.tag_photos.remove(&id) {
            for p in photos {
                if let Some(set) = self.photo_tags.get_mut(&p) {
                    set.remove(&id);
                    if set.is_empty() {
                        self.photo_tags.remove(&p);
                    }
                }
            }
        }
    }

    fn link(&mut self, tag: TagId, photo: PhotoId) {
        self.photo_tags.entry(photo).or_default().insert(tag);
        self.tag_photos.entry(tag).or_default().insert(photo);
    }

    fn unlink(&mut self, tag: TagId, photo: PhotoId) {
        if let Some(set) = self.photo_tags.get_mut(&photo) {
            set.remove(&tag);
            if set.is_empty() {
                self.photo_tags.remove(&photo);
            }
        }
        if let Some(set) = self.tag_photos.get_mut(&tag) {
            set.remove(&photo);
            if set.is_empty() {
                self.tag_photos.remove(&tag);
            }
        }
    }
}
