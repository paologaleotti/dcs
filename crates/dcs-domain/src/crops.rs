//! Crop + straighten: owned, non-destructive per-photo edit metadata, and the
//! pure geometry that turns it into pixel operations. No I/O, no egui — the same
//! math feeds the on-screen overlay, the display render, and the export render,
//! so the dialog preview and the real output can never diverge.
//!
//! Coordinate frame is the EXIF-oriented source image (orientation already
//! applied, like everywhere else). A [`CropEdit`] straightens by rotating the
//! image `angle_deg` about its center, then keeps the axis-aligned [`NormRect`]
//! window — expressed in the rotated image's bounding box, normalized 0..1.

use serde::{Deserialize, Serialize};

/// The straighten range in degrees, each direction. Beyond this a "straighten"
/// is really a re-compose; the slider and clamps honor it.
pub const MAX_ANGLE_DEG: f32 = 45.0;

/// A normalized axis-aligned rectangle, components in `0..=1`. The frame it's
/// normalized against depends on context; for [`CropEdit::rect`] it's the
/// rotated source's bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One photo's crop + straighten. `None` on a photo means uncropped (the
/// original). Owned state: persisted, undoable, true in every view.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CropEdit {
    /// Straighten angle, degrees, clockwise-positive, about the image center.
    /// Clamped to `[-MAX_ANGLE_DEG, MAX_ANGLE_DEG]`.
    pub angle_deg: f32,
    /// The crop window, axis-aligned in the rotated display frame, normalized to
    /// the rotated image's bounding box. Invariant kept by callers: its source
    /// quad lies within the image, so output never has empty corners.
    pub rect: NormRect,
}

/// The pixel recipe to produce a cropped+straightened image: rotate the source
/// by `angle_deg` into a `bbox_w × bbox_h` canvas (source centered), then crop
/// the `out_w × out_h` window at `(crop_x, crop_y)`. Both the display renderer
/// and the export executor consume this, so there is one pixel path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRender {
    pub bbox_w: u32,
    pub bbox_h: u32,
    pub crop_x: u32,
    pub crop_y: u32,
    pub out_w: u32,
    pub out_h: u32,
}

impl NormRect {
    /// The whole frame: `(0, 0, 1, 1)`.
    pub const FULL: NormRect = NormRect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    /// Centered rect of normalized size `(w, h)`.
    pub fn centered(w: f32, h: f32) -> NormRect {
        NormRect {
            x: (1.0 - w) * 0.5,
            y: (1.0 - h) * 0.5,
            w,
            h,
        }
    }

    /// The four corners (TL, TR, BR, BL) in normalized frame coords.
    pub fn corners(&self) -> [(f32, f32); 4] {
        [
            (self.x, self.y),
            (self.x + self.w, self.y),
            (self.x + self.w, self.y + self.h),
            (self.x, self.y + self.h),
        ]
    }
}

impl CropEdit {
    /// The identity edit: no rotation, full frame — equivalent to uncropped.
    pub fn identity() -> CropEdit {
        CropEdit {
            angle_deg: 0.0,
            rect: NormRect::FULL,
        }
    }

    /// Whether this edit changes nothing — zero angle and the full frame. A
    /// no-op edit is stored as `None` rather than persisted.
    pub fn is_noop(&self) -> bool {
        self.angle_deg.abs() < 1e-4
            && self.rect.x.abs() < 1e-4
            && self.rect.y.abs() < 1e-4
            && (self.rect.w - 1.0).abs() < 1e-4
            && (self.rect.h - 1.0).abs() < 1e-4
    }

    /// A stable 64-bit token of this edit, for keying derived caches (the disk
    /// thumbnail cache folds it into the content key so a cropped thumbnail is
    /// cached per distinct crop). Stable across runs — folds the raw bit patterns
    /// with FNV-1a, never a randomly-seeded hasher.
    pub fn cache_token(&self) -> u64 {
        let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
        let fields = [
            self.angle_deg.to_bits(),
            self.rect.x.to_bits(),
            self.rect.y.to_bits(),
            self.rect.w.to_bits(),
            self.rect.h.to_bits(),
        ];
        for word in fields {
            for byte in word.to_le_bytes() {
                acc ^= byte as u64;
                acc = acc.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        acc
    }

    /// Clamp the angle into range and the rect into `0..=1`.
    pub fn sanitized(self) -> CropEdit {
        let angle_deg = self.angle_deg.clamp(-MAX_ANGLE_DEG, MAX_ANGLE_DEG);
        let x = self.rect.x.clamp(0.0, 1.0);
        let y = self.rect.y.clamp(0.0, 1.0);
        let w = self.rect.w.clamp(0.0, 1.0 - x);
        let h = self.rect.h.clamp(0.0, 1.0 - y);
        CropEdit {
            angle_deg,
            rect: NormRect { x, y, w, h },
        }
    }
}

/// The rotated source's bounding-box pixel size for angle `angle_deg`.
pub fn bounding_box(src_w: u32, src_h: u32, angle_deg: f32) -> (f32, f32) {
    let (w, h) = (src_w as f32, src_h as f32);
    let a = angle_deg.to_radians();
    let (sin, cos) = (a.sin().abs(), a.cos().abs());
    (w * cos + h * sin, w * sin + h * cos)
}

/// Resolve a [`CropEdit`] against a source size into the concrete [`CropRender`].
/// Pure: the bounding box, crop offset, and output size are all derived, never
/// guessed. A degenerate (zero-area) window yields a `1×1` output rather than a
/// zero dimension.
pub fn plan_crop(src_w: u32, src_h: u32, edit: &CropEdit) -> CropRender {
    let (bbox_w, bbox_h) = bounding_box(src_w, src_h, edit.angle_deg);
    let r = edit.rect;
    let crop_x = (r.x * bbox_w).round().max(0.0);
    let crop_y = (r.y * bbox_h).round().max(0.0);
    let bw = bbox_w.round().max(1.0) as u32;
    let bh = bbox_h.round().max(1.0) as u32;
    let out_w = ((r.w * bbox_w).round() as u32).clamp(1, bw);
    let out_h = ((r.h * bbox_h).round() as u32).clamp(1, bh);
    CropRender {
        bbox_w: bw,
        bbox_h: bh,
        crop_x: (crop_x as u32).min(bw.saturating_sub(out_w)),
        crop_y: (crop_y as u32).min(bh.saturating_sub(out_h)),
        out_w,
        out_h,
    }
}

/// The largest axis-aligned (display-frame), centered rectangle that fits
/// entirely inside the source rotated by `angle_deg`, as a [`NormRect`] over the
/// rotated bounding box. This is the auto-inset bound the UI clamps to so a
/// straightened crop never exposes empty corners. At angle 0 it is the full
/// frame.
pub fn max_inset_rect(src_w: u32, src_h: u32, angle_deg: f32) -> NormRect {
    let (bbox_w, bbox_h) = bounding_box(src_w, src_h, angle_deg);
    let (wr, hr) = rotated_rect_with_max_area(src_w as f32, src_h as f32, angle_deg);
    let w = (wr / bbox_w).clamp(0.0, 1.0);
    let h = (hr / bbox_h).clamp(0.0, 1.0);
    NormRect::centered(w, h)
}

/// Fit a rectangle of pixel aspect ratio `ratio` (width / height), centered,
/// as large as possible within `bound` (itself a NormRect over the bounding
/// box). `bbox_w`/`bbox_h` give the box's pixel dimensions so the ratio is in
/// true pixels, not normalized units. Used by the aspect-ratio presets.
pub fn fit_aspect(bbox_w: f32, bbox_h: f32, bound: NormRect, ratio: f32) -> NormRect {
    if ratio <= 0.0 || bbox_w <= 0.0 || bbox_h <= 0.0 {
        return bound;
    }
    // Bound dimensions in pixels.
    let (bpw, bph) = (bound.w * bbox_w, bound.h * bbox_h);
    // Largest w×h with w/h == ratio fitting inside (bpw, bph).
    let (mut pw, mut ph) = (bpw, bpw / ratio);
    if ph > bph {
        ph = bph;
        pw = bph * ratio;
    }
    let w = pw / bbox_w;
    let h = ph / bbox_h;
    NormRect {
        x: bound.x + (bound.w - w) * 0.5,
        y: bound.y + (bound.h - h) * 0.5,
        w,
        h,
    }
}

/// True when every corner of `edit`'s crop window maps inside the source image
/// — i.e. the output contains no empty (off-image) pixels. The "no empty corners"
/// invariant the editor upholds indirectly (via `max_inset_rect` +
/// `clamp_rect_ratio`); kept here as the direct predicate the tests assert against.
pub fn is_within_source(src_w: u32, src_h: u32, edit: &CropEdit) -> bool {
    let quad = crop_quad_in_source(src_w, src_h, edit);
    let (w, h) = (src_w as f32, src_h as f32);
    let eps = 0.5; // half a pixel of slack for rounding
    quad.iter()
        .all(|&(px, py)| px >= -eps && py >= -eps && px <= w + eps && py <= h + eps)
}

/// The crop window's four corners (TL, TR, BR, BL) in source-pixel coordinates.
/// For tests and the UI's containment clamp.
pub fn crop_quad_in_source(src_w: u32, src_h: u32, edit: &CropEdit) -> [(f32, f32); 4] {
    let sampler = SourceSampler::new(src_w, src_h, edit.angle_deg);
    edit.rect
        .corners()
        .map(|(nx, ny)| sampler.source_at(nx, ny))
}

/// The single straighten transform: maps a point in normalized rotated
/// bounding-box coordinates (`(0,0)`..`(1,1)` over the box) back to source-pixel
/// coordinates, inverting the straighten rotation about the image center.
///
/// This is *the* crop math path. The UI overlay's containment clamp, the display
/// thumbnail renderer, and the export renderer all sample through this one
/// transform, so the preview can never diverge from the output. Construct once
/// per image (it precomputes the trig + bounding box) and call [`source_at`] per
/// point/pixel.
///
/// [`source_at`]: SourceSampler::source_at
#[derive(Debug, Clone, Copy)]
pub struct SourceSampler {
    bbox_w: f32,
    bbox_h: f32,
    sin: f32,
    cos: f32,
    half_src_w: f32,
    half_src_h: f32,
}

impl SourceSampler {
    /// Precompute the transform for a source size and straighten angle.
    pub fn new(src_w: u32, src_h: u32, angle_deg: f32) -> Self {
        let (bbox_w, bbox_h) = bounding_box(src_w, src_h, angle_deg);
        let a = angle_deg.to_radians();
        SourceSampler {
            bbox_w,
            bbox_h,
            sin: a.sin(),
            cos: a.cos(),
            half_src_w: src_w as f32 * 0.5,
            half_src_h: src_h as f32 * 0.5,
        }
    }

    /// The rotated bounding-box pixel size this sampler maps over.
    pub fn bbox(&self) -> (f32, f32) {
        (self.bbox_w, self.bbox_h)
    }

    /// Map a normalized bounding-box point `(nx, ny)` to source-pixel `(sx, sy)`.
    pub fn source_at(&self, nx: f32, ny: f32) -> (f32, f32) {
        // Bounding-box pixel coords, centered on the box (= image) center.
        let bx = nx * self.bbox_w - self.bbox_w * 0.5;
        let by = ny * self.bbox_h - self.bbox_h * 0.5;
        // Inverse-rotate by the straighten angle, then translate to source px.
        (
            bx * self.cos + by * self.sin + self.half_src_w,
            -bx * self.sin + by * self.cos + self.half_src_h,
        )
    }
}

/// The size of the largest upright rectangle that fits inside a `w × h`
/// rectangle rotated by `angle_deg`. Classic "rotate and crop out the black
/// borders" result; symmetric in the sign of the angle.
fn rotated_rect_with_max_area(w: f32, h: f32, angle_deg: f32) -> (f32, f32) {
    if w <= 0.0 || h <= 0.0 {
        return (0.0, 0.0);
    }
    let a = angle_deg.to_radians().abs();
    let sin = a.sin();
    let cos = a.cos();
    let width_is_longer = w >= h;
    let (long, short) = if width_is_longer { (w, h) } else { (h, w) };

    if short <= 2.0 * sin * cos * long || (sin - cos).abs() < 1e-10 {
        // Half-constrained: the crop touches the midpoints of the long sides.
        let x = 0.5 * short;
        let (wr, hr) = if width_is_longer {
            (
                if sin > 1e-10 { x / sin } else { long },
                if cos > 1e-10 { x / cos } else { short },
            )
        } else {
            (
                if cos > 1e-10 { x / cos } else { short },
                if sin > 1e-10 { x / sin } else { long },
            )
        };
        (wr, hr)
    } else {
        let cos_2a = cos * cos - sin * sin;
        let wr = (w * cos - h * sin) / cos_2a;
        let hr = (h * cos - w * sin) / cos_2a;
        (wr, hr)
    }
}

/// Clamp `v` into `[lo, hi]` without panicking when `hi < lo` (which a hair of
/// float rounding can produce) or on NaN. When the range is empty or non-finite
/// it pins to `lo`. `f32::clamp` panics on `lo > hi` or NaN — every clamp in this
/// module goes through here so a degenerate range can never crash the editor.
pub fn clamp_axis(v: f32, lo: f32, hi: f32) -> f32 {
    if !lo.is_finite() || !hi.is_finite() || !v.is_finite() || hi <= lo {
        return lo;
    }
    v.clamp(lo, hi)
}

/// Clamp `r` to lie within `bound`: shrink it to fit on each axis, then translate
/// it into range. Panic-safe and order-stable — a rect larger than `bound` pins
/// to `bound`'s origin rather than crashing.
pub fn clamp_rect(mut r: NormRect, bound: NormRect) -> NormRect {
    r.w = r.w.clamp(0.0, bound.w.max(0.0));
    r.h = r.h.clamp(0.0, bound.h.max(0.0));
    r.x = clamp_axis(r.x, bound.x, bound.x + bound.w - r.w);
    r.y = clamp_axis(r.y, bound.y, bound.y + bound.h - r.h);
    r
}

/// Clamp `r` into `bound` while preserving the normalized ratio `rn = w / h`.
/// The width is capped to the largest ratio box that fits `bound` (so neither
/// axis can overflow, even after rounding), then the rect is translated minimally
/// to sit inside `bound`. **Position-preserving — it does not recenter:** a
/// recenter would fight the editor's corner-anchored resize and stall growth
/// before the real limit. Panic-safe; falls back to a free clamp on a degenerate
/// ratio or bound.
pub fn clamp_rect_ratio(r: NormRect, bound: NormRect, rn: f32) -> NormRect {
    if rn <= 0.0 || !rn.is_finite() || bound.w <= 0.0 || bound.h <= 0.0 {
        return clamp_rect(r, bound);
    }
    let max_w = bound.w.min(bound.h * rn);
    let w = r.w.clamp(0.0, max_w);
    let h = w / rn;
    NormRect {
        x: clamp_axis(r.x, bound.x, bound.x + bound.w - w),
        y: clamp_axis(r.y, bound.y, bound.y + bound.h - h),
        w,
        h,
    }
}

/// Smallest crop window as a fraction of the bounding box — a drag can't collapse
/// the rect below this on either axis.
pub const MIN_CROP_FRAC: f32 = 0.05;

/// Which edges of the crop rect a drag handle moves. A corner sets two adjacent
/// sides, an edge one, a move none. The UI maps its handle to this; the resize
/// math stays pure and testable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DragSides {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// Resize `rect` by moving its `sides` to the normalized pointer `(nx, ny)`,
/// never crossing the opposite edge (a [`MIN_CROP_FRAC`] gap keeps it
/// non-degenerate). When `ratio_norm` is `Some(rn)` the result keeps that
/// normalized `w/h`, fitting the ratio box inside the dragged free box and
/// anchoring it on the handle's fixed edge(s) (an edge handle centers the
/// perpendicular axis). Pure — the editor's drag geometry, unit-tested here
/// rather than stranded in the UI.
pub fn drag_rect(
    rect: NormRect,
    sides: DragSides,
    nx: f32,
    ny: f32,
    ratio_norm: Option<f32>,
) -> NormRect {
    let (mut x0, mut x1, mut y0, mut y1) = (rect.x, rect.x + rect.w, rect.y, rect.y + rect.h);
    if sides.left {
        x0 = nx.min(x1 - MIN_CROP_FRAC);
    }
    if sides.right {
        x1 = nx.max(x0 + MIN_CROP_FRAC);
    }
    if sides.top {
        y0 = ny.min(y1 - MIN_CROP_FRAC);
    }
    if sides.bottom {
        y1 = ny.max(y0 + MIN_CROP_FRAC);
    }

    if let Some(rn) = ratio_norm.filter(|rn| *rn > 0.0 && rn.is_finite()) {
        let (fw, fh) = (x1 - x0, y1 - y0);
        let w = fw.min(fh * rn).max(MIN_CROP_FRAC);
        let h = (w / rn).max(MIN_CROP_FRAC);
        if sides.left {
            x0 = x1 - w;
        } else if sides.right {
            x1 = x0 + w;
        } else {
            let cx = (x0 + x1) * 0.5;
            x0 = cx - w * 0.5;
            x1 = cx + w * 0.5;
        }
        if sides.top {
            y0 = y1 - h;
        } else if sides.bottom {
            y1 = y0 + h;
        } else {
            let cy = (y0 + y1) * 0.5;
            y0 = cy - h * 0.5;
            y1 = cy + h * 0.5;
        }
    }
    NormRect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}
