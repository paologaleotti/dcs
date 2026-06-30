use dcs_domain::crops::CropEdit;
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::{Photo, PhotoId};
use dcs_domain::thumb::ThumbImage;
use dcs_domain::timezone;
use dcs_io::cache::ThumbTier;
use dcs_io::imaging::{DecodePriority, DecodeRequest, ThumbDecoder};

use crate::thumb_cache::ThumbView;
use crate::util::{DecodeTier, encode_key};

use super::{
    BASE_EDGE, BG_FILL_INFLIGHT, CaptionTime, CellInfo, GALLERY_FULL_EDGE, GALLERY_PREVIEW_EDGE,
    ImportProgress, Session,
};

impl Session {
    /// Number of cells the grid paints — visible photos after the verdict
    /// filter. Equals the pool size when filter = `All`.
    pub fn photo_count(&self) -> usize {
        self.visible.len()
    }

    /// Total photos imported, ignoring the filter. Lets the UI tell "no folder
    /// open" apart from "the filter hid everything".
    pub fn pool_len(&self) -> usize {
        self.builder.len()
    }

    /// Photos that *can* show on the grid, ignoring the filter — the pool minus
    /// RAW-only photos (which have nothing to display in v1). This is the honest
    /// denominator for "N of M shown": with no filter, `photo_count` equals it.
    pub fn displayable_count(&self) -> usize {
        self.builder
            .photos()
            .iter()
            .filter(|p| !p.is_raw_only())
            .count()
    }

    /// Photos that will actually be embedded for AI search — displayable minus
    /// missing placeholders (no pixels to embed). The honest denominator for the
    /// "indexing N/M" status, matching what [`Session::index_pool`] enqueues.
    pub(super) fn embeddable_count(&self) -> usize {
        self.builder
            .photos()
            .iter()
            .filter(|p| !p.missing && !p.is_raw_only())
            .count()
    }

    /// Photo at a display position in the current visible order.
    pub fn photo_at(&self, display_index: usize) -> Option<&Photo> {
        let &pool_index = self.visible.get(display_index)?;
        self.builder.photos().get(pool_index)
    }

    /// The derived group title a display position falls under, for the gallery
    /// caption. `None` for the headerless `none`-axis stream.
    pub fn group_title_at(&self, display_index: usize) -> Option<&str> {
        self.visible_groups
            .iter()
            .find(|g| display_index >= g.start && display_index < g.start + g.count)
            .map(|g| g.title.as_str())
            .filter(|t| !t.is_empty())
    }

    /// The capture time for the gallery caption: the time in the display (shoot)
    /// zone always, plus the raw EXIF shot time when it differs (so a timezone
    /// shift is visible). `None` when the photo is undated.
    pub fn caption_time(&self, display_index: usize) -> Option<CaptionTime> {
        let photo = self.photo_at(display_index)?;
        let naive = photo.captured_at?;
        let adjusted = timezone::attributed_instant(
            naive,
            photo.captured_offset,
            self.resolve_camera_zone(),
            self.resolve_display_zone(),
        );
        let adjusted_str = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            adjusted.year(),
            u8::from(adjusted.month()),
            adjusted.day(),
            adjusted.hour(),
            adjusted.minute(),
            adjusted.second()
        );
        let shot_str = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            naive.year(),
            u8::from(naive.month()),
            naive.day(),
            naive.hour(),
            naive.minute(),
            naive.second()
        );
        let offset = timezone::format_offset(adjusted.offset());
        let shot = (shot_str != adjusted_str).then_some(shot_str);
        Some(CaptionTime {
            adjusted: adjusted_str,
            offset,
            shot,
        })
    }

    /// The photo's EXIF capture offset as a `UTC±HH:MM` label. `None` when the
    /// photo carries no `OffsetTimeOriginal`.
    pub fn exif_offset(&self, display_index: usize) -> Option<String> {
        let offset = self.photo_at(display_index)?.captured_offset?;
        Some(format!("UTC{}", timezone::format_offset(offset)))
    }

    /// Every known fact about the photo at `display_index`, as ordered
    /// `(label, value)` rows for the metadata dialog. Only present fields appear.
    pub fn photo_metadata(&self, display_index: usize) -> Option<Vec<(&'static str, String)>> {
        let photo = self.photo_at(display_index)?;
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        rows.push(("File", photo.file_name()));
        let kind = match (photo.files.jpeg.is_some(), photo.files.raw.is_some()) {
            (true, true) => "RAW + JPEG",
            (false, true) => "RAW",
            _ => "JPEG",
        };
        rows.push(("Type", kind.to_string()));
        rows.push((
            "Verdict",
            match self.verdict(photo.id) {
                AcceptState::Accepted => "accepted",
                AcceptState::Rejected => "rejected",
                AcceptState::Unreviewed => "unreviewed",
            }
            .to_string(),
        ));
        if photo.missing {
            rows.push(("Status", "missing on disk".to_string()));
        }

        if let Some(camera) = &photo.meta.camera {
            rows.push(("Camera", camera.clone()));
        }
        if let Some(lens) = &photo.meta.lens {
            rows.push(("Lens", lens.clone()));
        }
        if let Some(focal) = photo.meta.focal_label() {
            rows.push(("Focal length", focal));
        }
        if let Some(aperture) = photo.meta.aperture_label() {
            rows.push(("Aperture", aperture));
        }
        if let Some(shutter) = photo.meta.shutter_label() {
            rows.push(("Shutter", shutter));
        }
        if let Some(iso) = photo.meta.iso_label() {
            rows.push(("ISO", iso));
        }

        if let Some(caption) = self.caption_time(display_index) {
            rows.push((
                "Time (travel)",
                format!("{} (UTC{})", caption.adjusted, caption.offset),
            ));
            if let Some(shot) = caption.shot {
                rows.push(("Shot (camera)", shot));
            }
        }
        if let Some(exif) = self.exif_offset(display_index) {
            rows.push(("EXIF offset", exif));
        }
        rows.push((
            "Travel zone",
            self.shoot_zone().unwrap_or("system default").to_string(),
        ));
        rows.push((
            "Camera zone",
            self.camera_zone().unwrap_or("system default").to_string(),
        ));
        if let Some(group) = self.group_title_at(display_index) {
            rows.push(("Group", group.to_string()));
        }

        if let Some(jpeg) = photo.files.jpeg.as_deref() {
            rows.push(("JPEG path", jpeg.display().to_string()));
        }
        if let Some(raw) = photo.files.raw.as_deref() {
            rows.push(("RAW path", raw.display().to_string()));
        }
        Some(rows)
    }

    /// Cheap `Copy` descriptor for painting a cell — no allocation, so the grid
    /// can call it for every visible cell each frame.
    pub fn cell_info(&self, display_index: usize) -> Option<CellInfo> {
        let &pool_index = self.visible.get(display_index)?;
        let photo = self.builder.photos().get(pool_index)?;
        Some(CellInfo {
            id: photo.id,
            raw_only: photo.is_raw_only(),
            state: self.cull.state(photo.id),
            selected: self.sel.is_selected(photo.id),
            missing: photo.missing,
            tag_colors: self.tags.strip(photo.id),
            burst: self.bursts.get(&photo.id).copied(),
            cropped: self.crops.has_crop(photo.id),
        })
    }

    /// Decode jobs currently in flight, base + hi-res (diagnostics).
    pub fn decode_queue_depth(&self) -> usize {
        self.base.inflight_len() + self.hires.inflight_len() + self.gallery.inflight_len()
    }

    /// Photos with a base thumbnail resident.
    pub fn loaded_count(&self) -> usize {
        self.base.len()
    }

    /// Resident hi-res thumbnails (diagnostics).
    pub fn hires_count(&self) -> usize {
        self.hires.len()
    }

    /// Resident thumbnail pixel memory in MB across both tiers.
    pub fn thumb_memory_mb(&self) -> f32 {
        (self.base.weight() + self.hires.weight() + self.gallery.weight()) as f32
            / (1024.0 * 1024.0)
    }

    /// True while there is decode work in flight — the UI uses this to keep
    /// repainting until the visible set settles.
    pub fn has_pending(&self) -> bool {
        self.base.pending() || self.hires.pending()
    }

    /// Ensure the cheap base (256 px) thumbnail for a display position is
    /// decoding or cached. Called for the whole visible + prefetch band every
    /// frame at foreground priority. No-op if cached, already decoding, or
    /// RAW-only.
    pub fn request_base(&mut self, display_index: usize) {
        self.request_base_at(display_index, DecodePriority::High);
    }

    /// Ensure a hi-res thumbnail covering `target_edge` on-screen pixels is
    /// decoding or cached. Called only for viewport cells while zoomed in.
    /// Re-decodes at a larger tier when the cell grew past the cached one.
    pub fn request_hires(&mut self, display_index: usize, target_edge: u32) {
        let Some(&pool_index) = self.visible.get(display_index) else {
            return;
        };
        if self.builder.photos()[pool_index].missing {
            return; // nothing to decode for a missing file (see `request_base`)
        }
        let id = self.builder.photos()[pool_index].id;
        if self.hires.is_inflight(id) {
            return;
        }
        // Already resident at or above the wanted size → nothing to do.
        if self
            .hires
            .view(id)
            .is_some_and(|v| v.image.width.max(v.image.height) >= target_edge)
        {
            return;
        }
        let photo = &self.builder.photos()[pool_index];
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        let crop = self.crops.crop_of(id);
        self.hires.start(id);
        // Hi-res is viewport-ephemeral and its size tracks the zoom, so it is
        // not disk-cached (no stable tier to key it on); it lives only in RAM
        // and is dropped on zoom-out.
        self.decoder.request(DecodeRequest {
            key: encode_key(self.epoch, self.crop_gen_key(id), id, DecodeTier::Hires),
            path,
            orientation,
            edge: target_edge,
            cache_key: None,
            tier: ThumbTier::Gallery,
            cache: None,
            priority: DecodePriority::High,
            crop,
        });
    }

    /// Drop all hi-res thumbnails — called on zoom-out so the sharp pixels
    /// stop costing RAM. Base thumbnails (the display fallback) are untouched.
    pub fn clear_hires(&mut self) {
        if self.hires.len() == 0 && !self.hires.pending() {
            return;
        }
        self.hires.reset(super::HIRES_CACHE_BYTES);
    }

    /// Ensure the gallery frame for a display position is decoding or resident,
    /// sized to cover `fit_edge` device pixels, and preload the two neighbours at
    /// the same size so `←`/`→` lands on a ready image. Called each frame
    /// while the gallery is open.
    pub fn request_gallery(&mut self, display_index: usize, fit_edge: u32) {
        // Cap to the preview tier: a fit decode never needs more than a
        // screen-class image, and bounding it keeps a long navigation session
        // from accumulating full-window frames in RAM.
        let edge = fit_edge.min(GALLERY_PREVIEW_EDGE);
        self.request_gallery_at(display_index, edge);
        if display_index > 0 {
            self.request_gallery_at(display_index - 1, edge);
        }
        self.request_gallery_at(display_index + 1, edge);
    }

    /// Request a 1:1 (native-resolution) decode of one photo, capped at the GPU
    /// texture limit — the `Z` zoom-to-100% path.
    pub fn request_gallery_full(&mut self, display_index: usize) {
        self.request_gallery_at(display_index, GALLERY_FULL_EDGE);
    }

    /// The resident gallery frame for a photo, if decoded. Marks it recently used.
    pub fn gallery_image(&mut self, id: PhotoId) -> Option<ThumbView<'_>> {
        self.gallery.view(id)
    }

    /// Drop every gallery frame — called on leaving the gallery so the large
    /// pixels stop costing RAM. Base/hi-res caches are untouched.
    pub fn clear_gallery(&mut self) {
        if self.gallery.len() == 0 && !self.gallery.pending() {
            return;
        }
        self.gallery.reset(super::GALLERY_CACHE_BYTES);
        self.gallery_edge.clear();
    }

    /// True while a gallery frame is still decoding — the UI keeps repainting
    /// until the visible frame resolves.
    pub fn has_gallery_pending(&self) -> bool {
        self.gallery.pending()
    }

    /// Ensure a sharp board frame for `id` covering `target_edge` on-screen
    /// pixels is decoding or resident. Keyed by `PhotoId` (board membership is
    /// independent of the visible grid order) and capped at [`BOARD_MAX_EDGE`].
    /// The wanted size is **quantized to a tier ladder** so a continuous zoom-in
    /// steps through a handful of decodes rather than re-decoding every frame;
    /// the GPU bilinear-scales between tiers, so steady viewing and zoom-out
    /// never re-decode (the Figma/canvas approach). RAM-only.
    pub fn request_board(&mut self, id: PhotoId, target_edge: u32) {
        let edge = board_tier(target_edge);
        if self.board_cache.is_inflight(id) {
            return;
        }
        if self.board_edge.get(&id).copied().unwrap_or(0) >= edge {
            return;
        }
        let Some(pool_index) = self.pool_index_of(id) else {
            return;
        };
        let photo = &self.builder.photos()[pool_index];
        if photo.missing {
            return;
        }
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        let crop = self.crops.crop_of(id);
        self.board_edge.insert(id, edge);
        self.board_cache.start(id);
        self.decoder.request(DecodeRequest {
            key: encode_key(self.epoch, self.crop_gen_key(id), id, DecodeTier::Board),
            path,
            orientation,
            edge,
            cache_key: None,
            tier: ThumbTier::Gallery,
            cache: None,
            priority: DecodePriority::High,
            crop,
        });
    }

    /// The resident sharp board frame for a photo if present, else the base
    /// thumbnail — resolved in one session borrow so the canvas paints the
    /// sharpest available source. Marks the hit recently used.
    pub fn board_or_thumb(&mut self, id: PhotoId) -> Option<ThumbView<'_>> {
        if self.board_cache.contains(id) {
            return self.board_cache.view(id);
        }
        self.thumb(id)
    }

    /// The pool index for a `PhotoId`, via a revision-keyed cache so the board's
    /// per-frame lookups are O(1) rather than a full-pool scan. Rebuilt only when
    /// the pool changes (scan/forget bump the revision).
    fn pool_index_of(&mut self, id: PhotoId) -> Option<usize> {
        if self.id_index_rev != Some(self.builder.revision()) {
            self.id_index = self
                .builder
                .photos()
                .iter()
                .enumerate()
                .map(|(i, p)| (p.id, i))
                .collect();
            self.id_index_rev = Some(self.builder.revision());
        }
        self.id_index.get(&id).copied()
    }

    /// Drop every board frame — called on leaving the board so the large pixels
    /// stop costing RAM. Base/hi-res/gallery caches are untouched.
    pub fn clear_board(&mut self) {
        if self.board_cache.len() == 0 && !self.board_cache.pending() {
            return;
        }
        self.board_cache.reset(super::BOARD_CACHE_BYTES);
        self.board_edge.clear();
    }

    /// True while a board frame is still decoding — the UI keeps repainting until
    /// the visible items resolve.
    pub fn has_board_pending(&self) -> bool {
        self.board_cache.pending()
    }

    /// Whether the background fill still has folder left to walk — so the UI
    /// keeps repainting to drive it even when fully idle.
    pub fn has_background_work(&self) -> bool {
        self.bg_cursor < self.visible.len()
    }

    /// Walk the whole folder once in the background, decoding each thumbnail so
    /// it lands in the on-disk cache — then scrolling anywhere is a fast cached
    /// decode, never a cold full-resolution read. The RAM cache is LRU-bounded,
    /// so this warms the disk for the whole folder without holding it all in
    /// memory. Throttled to a small in-flight count and run at low decode
    /// priority so viewport and gallery decodes always win.
    pub fn fill_base_background(&mut self) {
        while self.base.inflight_len() < BG_FILL_INFLIGHT && self.bg_cursor < self.visible.len() {
            let index = self.bg_cursor;
            self.bg_cursor += 1;
            // Already warm on disk — re-decoding it would waste work. The
            // viewport still loads it into RAM on demand when scrolled to.
            if self
                .visible_photo_id(index)
                .is_some_and(|id| self.imported.contains(&id))
            {
                continue;
            }
            self.request_base_at(index, DecodePriority::Low);
        }
    }

    /// Background import progress — displayable thumbnails warmed into the disk
    /// cache out of the displayable total. `None` while scanning or once every
    /// displayable photo is warm, so the UI hides the bar then.
    pub fn import_progress(&self) -> Option<ImportProgress> {
        if self.is_scanning() {
            return None;
        }
        let photos = self.builder.photos();
        let mut total = 0;
        let mut done = 0;
        for &pool_index in &self.visible {
            let photo = &photos[pool_index];
            if photo.missing {
                continue; // a placeholder has no pixels to import
            }
            total += 1;
            if self.imported.contains(&photo.id) {
                done += 1;
            }
        }
        if total == 0 || done >= total {
            return None;
        }
        Some(ImportProgress { done, total })
    }

    /// The best resident thumbnail for a photo — hi-res if present, else base —
    /// with its version. Marks it recently used so it survives eviction.
    pub fn thumb(&mut self, id: PhotoId) -> Option<ThumbView<'_>> {
        if let Some(view) = self.hires.view(id) {
            return Some(view);
        }
        self.base.view(id)
    }

    /// Core base request, parameterized by scheduling lane: the viewport/filmstrip
    /// use `High`, the whole-folder background fill uses `Low`.
    fn request_base_at(&mut self, display_index: usize, priority: DecodePriority) {
        let Some(&pool_index) = self.visible.get(display_index) else {
            return;
        };
        // A missing file has no pixels; its decode always fails and so never
        // caches, which would re-request it every frame. Skip it.
        if self.builder.photos()[pool_index].missing {
            return;
        }
        let id = self.builder.photos()[pool_index].id;
        if !self.base.idle(id) {
            return;
        }
        let photo = &self.builder.photos()[pool_index];
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        // The grid (base) tier is disk-cached by content fingerprint, so a
        // reopened folder paints from cached blobs instead of re-decoding.
        let cache_key = Some(photo.fingerprint);
        let crop = self.crops.crop_of(id);
        self.base.start(id);
        self.decoder.request(DecodeRequest {
            key: encode_key(self.epoch, self.crop_gen_key(id), id, DecodeTier::Base),
            path,
            orientation,
            edge: BASE_EDGE,
            cache_key,
            tier: ThumbTier::Grid,
            cache: self.cache.clone(),
            priority,
            crop,
        });
    }

    /// Request an *uncropped* full gallery frame for the crop editor, sized to
    /// `fit_edge` device pixels. The editor draws its overlay over the whole
    /// original image, so it never bakes the committed crop. Shares the gallery
    /// cache, so callers clear it on entering/leaving the editor (as the gallery
    /// itself does) to avoid a cropped/uncropped collision on one `PhotoId`.
    pub fn request_crop_source(&mut self, display_index: usize, fit_edge: u32) {
        let edge = fit_edge.min(GALLERY_PREVIEW_EDGE);
        self.request_gallery_core(display_index, edge, None);
    }

    /// Core gallery decode request: decode `display_index` at `edge` px on its
    /// longest side unless an at-least-as-large frame is already resident or in
    /// flight. Bakes `crop` into the pixels when set. Not disk-cached — gallery
    /// frames are large and ephemeral.
    fn request_gallery_at(&mut self, display_index: usize, edge: u32) {
        let Some(&pool_index) = self.visible.get(display_index) else {
            return;
        };
        let crop = self.crops.crop_of(self.builder.photos()[pool_index].id);
        self.request_gallery_core(display_index, edge, crop);
    }

    fn request_gallery_core(&mut self, display_index: usize, edge: u32, crop: Option<CropEdit>) {
        let Some(&pool_index) = self.visible.get(display_index) else {
            return;
        };
        if self.builder.photos()[pool_index].missing {
            return;
        }
        let id = self.builder.photos()[pool_index].id;
        if self.gallery.is_inflight(id) {
            return;
        }
        // `gallery_edge` is cleared on eviction, so a recorded edge ≥ target
        // means the frame is still resident (or in flight) at that size.
        if self.gallery_edge.get(&id).copied().unwrap_or(0) >= edge {
            return;
        }
        let photo = &self.builder.photos()[pool_index];
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        self.gallery_edge.insert(id, edge);
        self.gallery.start(id);
        self.decoder.request(DecodeRequest {
            key: encode_key(self.epoch, self.crop_gen_key(id), id, DecodeTier::Gallery),
            path,
            orientation,
            edge,
            cache_key: None,
            tier: ThumbTier::Gallery,
            cache: None,
            priority: DecodePriority::High,
            crop,
        });
    }

    /// Stable id at a display position, without the per-cell verdict/selection
    /// lookups `cell_info` does — cheap enough for the background-fill loop.
    fn visible_photo_id(&self, display_index: usize) -> Option<PhotoId> {
        let &pool_index = self.visible.get(display_index)?;
        Some(self.builder.photos()[pool_index].id)
    }

    /// Seed the import baseline from the disk cache: every displayable photo
    /// whose grid thumbnail is already stored counts as imported, so reopening a
    /// folder resumes the warm-up rather than restarting it. A cache read
    /// failure leaves the set empty — the import simply re-warms, never wrong.
    pub(super) fn seed_imported(&mut self) {
        self.imported.clear();
        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        let Ok(guard) = cache.lock() else {
            return;
        };
        let cached = guard.cached_keys(ThumbTier::Grid);
        drop(guard);
        for photo in self.builder.photos() {
            if photo.missing || photo.is_raw_only() {
                continue;
            }
            if cached.contains(photo.fingerprint.as_bytes()) {
                self.imported.insert(photo.id);
            }
        }
    }

    /// Retire one photo's in-flight marker in a tier without storing pixels —
    /// for a stale decode discarded at the epoch check. Keeps resident pixels.
    pub(super) fn retire_inflight(&mut self, id: PhotoId, tier: DecodeTier) {
        match tier {
            DecodeTier::Base => self.base.fail(id),
            DecodeTier::Hires => self.hires.fail(id),
            DecodeTier::Gallery => {
                self.gallery.fail(id);
                self.gallery_edge.remove(&id);
            }
            DecodeTier::Board => {
                self.board_cache.fail(id);
                self.board_edge.remove(&id);
            }
        }
    }

    /// Invalidate one photo's cached thumbnails across every tier after its crop
    /// changed, and bump *that photo's* decode generation so an in-flight decode of
    /// the old crop is discarded on arrival rather than stored. Scoped to the one
    /// photo — other photos' in-flight decodes are untouched. The next frame
    /// re-requests this photo, baking in the new crop.
    pub(super) fn invalidate_photo_thumbs(&mut self, id: PhotoId) {
        *self.crop_gen.entry(id).or_insert(0) += 1;
        self.base.invalidate(id);
        self.hires.invalidate(id);
        self.gallery.invalidate(id);
        self.gallery_edge.remove(&id);
        self.board_cache.invalidate(id);
        self.board_edge.remove(&id);
    }

    /// Fold a decode result into its tier's cache, retiring the in-flight marker
    /// (even on a failed decode). For the gallery tier, the per-photo edge record
    /// is pruned in lockstep with eviction so a re-entered photo re-decodes.
    pub(super) fn absorb_thumb(
        &mut self,
        id: PhotoId,
        tier: DecodeTier,
        image: Option<ThumbImage>,
    ) {
        let Some(image) = image else {
            match tier {
                DecodeTier::Base => self.base.fail(id),
                DecodeTier::Hires => self.hires.fail(id),
                DecodeTier::Gallery => {
                    self.gallery.fail(id);
                    self.gallery_edge.remove(&id);
                }
                DecodeTier::Board => {
                    self.board_cache.fail(id);
                    self.board_edge.remove(&id);
                }
            }
            return;
        };
        self.next_version += 1;
        let version = self.next_version;
        let evicted = match tier {
            DecodeTier::Base => {
                // A successful base decode is also written to the disk cache, so
                // it now counts toward the import — even after RAM eviction.
                self.imported.insert(id);
                self.base.store(id, image, version)
            }
            DecodeTier::Hires => self.hires.store(id, image, version),
            DecodeTier::Gallery => self.gallery.store(id, image, version),
            DecodeTier::Board => self.board_cache.store(id, image, version),
        };
        match tier {
            DecodeTier::Gallery => {
                for id in evicted {
                    self.gallery_edge.remove(&id);
                }
            }
            DecodeTier::Board => {
                for id in evicted {
                    self.board_edge.remove(&id);
                }
            }
            _ => {}
        }
    }
}

/// Quantize a wanted on-screen edge up to a fixed board-decode tier, so a
/// continuous zoom-in steps through a handful of decode sizes instead of
/// re-decoding at a new arbitrary pixel count every frame. Capped at the board
/// maximum; values above the top rung resolve there.
fn board_tier(edge: u32) -> u32 {
    const TIERS: [u32; 5] = [512, 768, 1152, 1728, 2560];
    TIERS
        .into_iter()
        .find(|&t| t >= edge)
        .unwrap_or(super::BOARD_MAX_EDGE)
}
