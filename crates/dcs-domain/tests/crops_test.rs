//! Exhaustive tests for the pure crop geometry: bounding box, plan_crop output
//! sizing, the auto-inset bound, aspect fitting, and the source-containment
//! invariant. Zero mocks — the module is pure math.

use dcs_domain::crops::{
    CropEdit, MAX_ANGLE_DEG, NormRect, SourceSampler, bounding_box, crop_quad_in_source,
    fit_aspect, is_within_source, max_inset_rect, plan_crop,
};

const EPS: f32 = 1e-3;

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() <= EPS
}

#[test]
fn identity_is_a_noop_and_full_frame() {
    let e = CropEdit::identity();
    assert!(e.is_noop());
    assert_eq!(e.rect, NormRect::FULL);
    assert_eq!(e.angle_deg, 0.0);
}

#[test]
fn a_real_crop_is_not_a_noop() {
    assert!(
        !CropEdit {
            angle_deg: 0.0,
            rect: NormRect::centered(0.5, 0.5),
        }
        .is_noop()
    );
    assert!(
        !CropEdit {
            angle_deg: 3.0,
            rect: NormRect::FULL,
        }
        .is_noop()
    );
}

#[test]
fn zero_angle_bounding_box_is_the_source() {
    let (w, h) = bounding_box(4000, 3000, 0.0);
    assert!(approx(w, 4000.0));
    assert!(approx(h, 3000.0));
}

#[test]
fn rotated_bounding_box_grows() {
    let (w, h) = bounding_box(4000, 3000, 90.0);
    // A 90° turn swaps the dimensions.
    assert!(approx(w, 3000.0));
    assert!(approx(h, 4000.0));
    let (w45, h45) = bounding_box(4000, 3000, 45.0);
    assert!(w45 > 4000.0 && h45 > 3000.0);
}

#[test]
fn plan_crop_identity_passes_the_whole_image_through() {
    let r = plan_crop(4000, 3000, &CropEdit::identity());
    assert_eq!(r.bbox_w, 4000);
    assert_eq!(r.bbox_h, 3000);
    assert_eq!(r.crop_x, 0);
    assert_eq!(r.crop_y, 0);
    assert_eq!(r.out_w, 4000);
    assert_eq!(r.out_h, 3000);
}

#[test]
fn plan_crop_half_rect_yields_half_output_centered() {
    let e = CropEdit {
        angle_deg: 0.0,
        rect: NormRect::centered(0.5, 0.5),
    };
    let r = plan_crop(4000, 3000, &e);
    assert_eq!(r.out_w, 2000);
    assert_eq!(r.out_h, 1500);
    assert_eq!(r.crop_x, 1000);
    assert_eq!(r.crop_y, 750);
}

#[test]
fn plan_crop_never_exceeds_the_bounding_box() {
    for angle in [-45.0, -12.3, 0.0, 7.0, 30.0, 45.0] {
        let e = CropEdit {
            angle_deg: angle,
            rect: NormRect::FULL,
        };
        let r = plan_crop(4000, 3000, &e);
        assert!(r.crop_x + r.out_w <= r.bbox_w, "x overflow at {angle}");
        assert!(r.crop_y + r.out_h <= r.bbox_h, "y overflow at {angle}");
        assert!(r.out_w >= 1 && r.out_h >= 1);
    }
}

#[test]
fn degenerate_rect_clamps_to_one_pixel() {
    let e = CropEdit {
        angle_deg: 0.0,
        rect: NormRect {
            x: 0.5,
            y: 0.5,
            w: 0.0,
            h: 0.0,
        },
    };
    let r = plan_crop(100, 100, &e);
    assert_eq!(r.out_w, 1);
    assert_eq!(r.out_h, 1);
}

#[test]
fn max_inset_at_zero_is_the_full_frame() {
    let r = max_inset_rect(4000, 3000, 0.0);
    assert!(approx(r.x, 0.0) && approx(r.y, 0.0));
    assert!(approx(r.w, 1.0) && approx(r.h, 1.0));
}

#[test]
fn max_inset_is_centered_and_inside_the_source_for_every_angle() {
    for angle in [-45.0, -33.0, -10.0, -0.5, 0.5, 10.0, 33.0, 45.0] {
        let inset = max_inset_rect(4000, 3000, angle);
        // Centered.
        assert!(
            approx(inset.x, (1.0 - inset.w) * 0.5),
            "off-center x at {angle}"
        );
        assert!(
            approx(inset.y, (1.0 - inset.h) * 0.5),
            "off-center y at {angle}"
        );
        // Every corner of the inset crop lands inside the source — no empty
        // corners.
        let e = CropEdit {
            angle_deg: angle,
            rect: inset,
        };
        assert!(
            is_within_source(4000, 3000, &e),
            "inset escapes source at {angle}"
        );
    }
}

#[test]
fn a_crop_larger_than_the_inset_escapes_the_source() {
    // The full frame rotated 20° must expose empty corners.
    let e = CropEdit {
        angle_deg: 20.0,
        rect: NormRect::FULL,
    };
    assert!(!is_within_source(4000, 3000, &e));
}

#[test]
fn crop_quad_maps_full_rect_to_the_image_corners_at_zero_angle() {
    let q = crop_quad_in_source(4000, 3000, &CropEdit::identity());
    assert!(approx(q[0].0, 0.0) && approx(q[0].1, 0.0)); // TL
    assert!(approx(q[2].0, 4000.0) && approx(q[2].1, 3000.0)); // BR
}

#[test]
fn source_sampler_at_zero_angle_maps_the_box_to_the_source() {
    let s = SourceSampler::new(4000, 3000, 0.0);
    assert_eq!(s.bbox(), (4000.0, 3000.0));
    // Box center → image center; corners → image corners; midpoints linear.
    let (cx, cy) = s.source_at(0.5, 0.5);
    assert!(approx(cx, 2000.0) && approx(cy, 1500.0));
    let (tlx, tly) = s.source_at(0.0, 0.0);
    assert!(approx(tlx, 0.0) && approx(tly, 0.0));
    let (brx, bry) = s.source_at(1.0, 1.0);
    assert!(approx(brx, 4000.0) && approx(bry, 3000.0));
}

#[test]
fn source_sampler_center_is_invariant_under_rotation() {
    // The straighten rotates about the image center, so the box center always maps
    // to the image center regardless of angle.
    for angle in [-45.0, -12.0, 0.0, 7.5, 45.0] {
        let s = SourceSampler::new(6000, 4000, angle);
        let (cx, cy) = s.source_at(0.5, 0.5);
        assert!(approx(cx, 3000.0), "cx {cx} at {angle}");
        assert!(approx(cy, 2000.0), "cy {cy} at {angle}");
    }
}

#[test]
fn source_sampler_is_the_quad_transform() {
    // crop_quad_in_source must be exactly the sampler at the rect corners — i.e.
    // there is one transform, not two. (This is the load-bearing "one math path".)
    let edit = CropEdit {
        angle_deg: 6.0,
        rect: NormRect::centered(0.7, 0.55),
    };
    let s = SourceSampler::new(6000, 4000, edit.angle_deg);
    let quad = crop_quad_in_source(6000, 4000, &edit);
    for (corner, &(qx, qy)) in edit.rect.corners().iter().zip(quad.iter()) {
        let (sx, sy) = s.source_at(corner.0, corner.1);
        assert!(
            approx(sx, qx) && approx(sy, qy),
            "sampler != quad at {corner:?}"
        );
    }
}

#[test]
fn fit_aspect_produces_the_requested_pixel_ratio() {
    let (bw, bh) = (4000.0, 3000.0);
    let bound = NormRect::FULL;
    // 1:1 square, centered.
    let sq = fit_aspect(bw, bh, bound, 1.0);
    let (pw, ph) = (sq.w * bw, sq.h * bh);
    assert!(approx(pw, ph), "square not square: {pw} vs {ph}");
    assert!(pw <= bh + EPS); // limited by the short side
    // 16:9 wide.
    let wide = fit_aspect(bw, bh, bound, 16.0 / 9.0);
    let (pw, ph) = (wide.w * bw, wide.h * bh);
    assert!(approx(pw / ph, 16.0 / 9.0));
}

#[test]
fn fit_aspect_stays_within_the_bound() {
    let bound = NormRect::centered(0.6, 0.6);
    let r = fit_aspect(4000.0, 3000.0, bound, 3.0 / 2.0);
    assert!(r.x >= bound.x - EPS);
    assert!(r.y >= bound.y - EPS);
    assert!(r.x + r.w <= bound.x + bound.w + EPS);
    assert!(r.y + r.h <= bound.y + bound.h + EPS);
}

#[test]
fn sanitized_clamps_angle_and_rect() {
    let e = CropEdit {
        angle_deg: 200.0,
        rect: NormRect {
            x: -0.2,
            y: 0.5,
            w: 5.0,
            h: 5.0,
        },
    }
    .sanitized();
    assert_eq!(e.angle_deg, MAX_ANGLE_DEG);
    assert!(e.rect.x >= 0.0 && e.rect.y >= 0.0);
    assert!(e.rect.x + e.rect.w <= 1.0 + EPS);
    assert!(e.rect.y + e.rect.h <= 1.0 + EPS);
}

// --- Panic-safe clamping (regression for the straighten crash) --------------

use dcs_domain::crops::{clamp_axis, clamp_rect, clamp_rect_ratio};

fn within(r: NormRect, b: NormRect) -> bool {
    let e = 1e-3;
    r.x >= b.x - e
        && r.y >= b.y - e
        && r.x + r.w <= b.x + b.w + e
        && r.y + r.h <= b.y + b.h + e
        && r.w >= -e
        && r.h >= -e
        && r.x.is_finite()
        && r.y.is_finite()
        && r.w.is_finite()
        && r.h.is_finite()
}

#[test]
fn clamp_axis_never_panics_on_inverted_or_nan_range() {
    // Inverted range (hi < lo by a float hair) pins to lo — the exact shape that
    // crashed: min = 0.12807676, max = 0.12807673.
    assert_eq!(clamp_axis(0.5, 0.12807676, 0.12807673), 0.12807676);
    assert_eq!(clamp_axis(5.0, 0.0, -1.0), 0.0);
    // NaN anywhere → returns lo, never a panic.
    assert_eq!(clamp_axis(f32::NAN, 0.0, 1.0), 0.0);
    assert!(clamp_axis(0.5, f32::NAN, 1.0).is_nan()); // lo is NaN → returns lo
    // Normal clamp still works.
    assert_eq!(clamp_axis(0.5, 0.0, 1.0), 0.5);
    assert_eq!(clamp_axis(2.0, 0.0, 1.0), 1.0);
    assert_eq!(clamp_axis(-2.0, 0.0, 1.0), 0.0);
}

#[test]
fn clamp_rect_keeps_an_oversized_rect_inside_the_bound() {
    let bound = NormRect::centered(0.6, 0.6);
    // A rect bigger than the bound on both axes.
    let r = NormRect {
        x: -0.5,
        y: -0.5,
        w: 2.0,
        h: 2.0,
    };
    let c = clamp_rect(r, bound);
    assert!(within(c, bound), "clamped rect escapes bound: {c:?}");
}

#[test]
fn clamp_rect_ratio_fits_and_preserves_ratio_for_a_mismatched_bound() {
    // A wide bound, a tall-ish ratio: the fit must shrink width, not overflow.
    let bound = NormRect {
        x: 0.1,
        y: 0.2,
        w: 0.8,
        h: 0.5,
    };
    let rn = 0.5; // w/h = 0.5 (portrait-ish in normalized space)
    let r = NormRect::centered(1.0, 1.0); // ask for the whole frame
    let c = clamp_rect_ratio(r, bound, rn);
    assert!(within(c, bound), "ratio clamp escapes bound: {c:?}");
    assert!((c.w / c.h - rn).abs() < 1e-3, "ratio not preserved: {c:?}");
}

#[test]
fn clamp_rect_ratio_never_panics_across_every_straighten_angle() {
    // The exact editor path that crashed: for a source, sweep the straighten
    // angle, derive the inset + normalized ratio, and clamp the full frame to it.
    // None of these may panic, and every result must sit inside its inset.
    let (sw, sh) = (6000u32, 4000u32);
    let src_ratio = sw as f32 / sh as f32;
    for ratios in [src_ratio, 1.0, 3.0 / 2.0, 4.0 / 3.0, 16.0 / 9.0, 9.0 / 16.0] {
        let mut angle = -MAX_ANGLE_DEG;
        while angle <= MAX_ANGLE_DEG {
            let inset = max_inset_rect(sw, sh, angle);
            let (bw, bh) = bounding_box(sw, sh, angle);
            let rn = ratios * bh / bw;
            // Start from the inset's own fit, then ask to clamp the full frame —
            // mimics a drag that pushed past the bound while straightening.
            let fitted = fit_aspect(bw, bh, inset, ratios);
            for start in [fitted, NormRect::FULL, NormRect::centered(0.99, 0.99)] {
                let c = clamp_rect_ratio(start, inset, rn);
                assert!(
                    within(c, inset),
                    "angle {angle} ratio {ratios}: {c:?} escapes inset {inset:?}"
                );
            }
            angle += 0.13; // a fine, irregular step to hit rounding edges
        }
    }
}

#[test]
fn clamp_rect_ratio_leaves_a_fitting_rect_where_it_is() {
    // Regression: the clamp must NOT recenter a rect that already fits — a
    // recenter fought the corner-anchored resize and stalled growth before the
    // real limit ("won't resize even with space"). A corner-anchored,
    // ratio-correct rect well inside the bound must come back unchanged.
    let bound = NormRect {
        x: 0.05,
        y: 0.05,
        w: 0.9,
        h: 0.9,
    };
    let rn = 1.5;
    // Anchored at the bound's top-left, half-size, correct ratio (0.45/0.30=1.5).
    let r = NormRect {
        x: 0.05,
        y: 0.05,
        w: 0.45,
        h: 0.30,
    };
    let c = clamp_rect_ratio(r, bound, rn);
    assert!((c.x - r.x).abs() < 1e-4, "x moved: {c:?}");
    assert!((c.y - r.y).abs() < 1e-4, "y moved: {c:?}");
    assert!((c.w - r.w).abs() < 1e-4, "w changed: {c:?}");
    assert!((c.h - r.h).abs() < 1e-4, "h changed: {c:?}");
}

#[test]
fn clamp_rect_ratio_lets_a_corner_anchored_box_grow_toward_the_limit() {
    // Simulate the editor's per-frame loop: a box anchored at its top-left grows
    // its width each frame (corner drag). The clamp must let it keep growing —
    // position fixed, size monotonically increasing — until the ratio limit, not
    // stall partway.
    let bound = NormRect {
        x: 0.05,
        y: 0.05,
        w: 0.9,
        h: 0.7,
    };
    let rn = 1.5;
    let max_w = bound.w.min(bound.h * rn);
    let mut w = 0.2;
    let mut last = clamp_rect_ratio(
        NormRect {
            x: bound.x,
            y: bound.y,
            w,
            h: w / rn,
        },
        bound,
        rn,
    );
    for _ in 0..40 {
        w += 0.05; // drag the corner out a notch
        let c = clamp_rect_ratio(
            NormRect {
                x: bound.x,
                y: bound.y,
                w,
                h: w / rn,
            },
            bound,
            rn,
        );
        // Anchor preserved, width never shrinks, ratio held.
        assert!((c.x - bound.x).abs() < 1e-4, "anchor x drifted: {c:?}");
        assert!(
            c.w + 1e-4 >= last.w,
            "growth stalled: {} -> {}",
            last.w,
            c.w
        );
        assert!((c.w / c.h - rn).abs() < 1e-3, "ratio broke: {c:?}");
        assert!(within(c, bound));
        last = c;
    }
    // Reached (about) the geometric limit, not stuck small.
    assert!(last.w >= max_w - 1e-3, "never reached the limit: {last:?}");
}

#[test]
fn clamp_rect_ratio_falls_back_safely_on_degenerate_input() {
    let bound = NormRect::centered(0.5, 0.5);
    // Zero / negative / non-finite ratio → free clamp, still inside, no panic.
    for rn in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let c = clamp_rect_ratio(NormRect::FULL, bound, rn);
        assert!(within(c, bound), "degenerate rn {rn} escaped: {c:?}");
    }
}

// --- drag_rect: editor resize geometry (pure, moved down from the UI) --------

use dcs_domain::crops::{DragSides, drag_rect};

const ALL_FREE: Option<f32> = None;

fn sides(l: bool, r: bool, t: bool, b: bool) -> DragSides {
    DragSides {
        left: l,
        right: r,
        top: t,
        bottom: b,
    }
}

#[test]
fn drag_free_corner_moves_only_the_dragged_corner() {
    // Bottom-right corner: top-left stays fixed, BR follows the pointer.
    let r = NormRect {
        x: 0.2,
        y: 0.2,
        w: 0.3,
        h: 0.3,
    };
    let out = drag_rect(r, sides(false, true, false, true), 0.9, 0.8, ALL_FREE);
    assert!(
        approx(out.x, 0.2) && approx(out.y, 0.2),
        "anchor moved: {out:?}"
    );
    assert!(approx(out.x + out.w, 0.9), "right edge: {out:?}");
    assert!(approx(out.y + out.h, 0.8), "bottom edge: {out:?}");
}

#[test]
fn drag_free_edge_moves_one_side() {
    let r = NormRect::centered(0.4, 0.4); // x=0.3,y=0.3,w=0.4,h=0.4
    let out = drag_rect(r, sides(true, false, false, false), 0.1, 0.5, ALL_FREE);
    assert!(approx(out.x, 0.1), "left edge moved: {out:?}");
    assert!(approx(out.x + out.w, 0.7), "right edge fixed: {out:?}");
    assert!(
        approx(out.y, 0.3) && approx(out.h, 0.4),
        "vertical untouched: {out:?}"
    );
}

#[test]
fn drag_cannot_collapse_past_min_frac() {
    let r = NormRect {
        x: 0.2,
        y: 0.2,
        w: 0.4,
        h: 0.4,
    };
    // Drag the right edge back past the left edge → clamped to a MIN_CROP_FRAC gap.
    let out = drag_rect(r, sides(false, true, false, false), 0.0, 0.5, ALL_FREE);
    assert!(
        out.w >= dcs_domain::crops::MIN_CROP_FRAC - 1e-4,
        "collapsed: {out:?}"
    );
}

#[test]
fn drag_with_ratio_lock_keeps_ratio_anchored_at_the_fixed_corner() {
    // Bottom-right drag, square lock (rn = 1): the rect stays square, anchored at
    // the fixed top-left corner.
    let r = NormRect {
        x: 0.2,
        y: 0.2,
        w: 0.2,
        h: 0.2,
    };
    let out = drag_rect(r, sides(false, true, false, true), 0.9, 0.7, Some(1.0));
    assert!(
        approx(out.x, 0.2) && approx(out.y, 0.2),
        "anchor moved: {out:?}"
    );
    assert!(approx(out.w, out.h), "not square: {out:?}");
    // Width fits inside the dragged free box (min of the two extents).
    assert!(out.w <= 0.5 + 1e-4 && out.w >= 0.49, "size: {out:?}");
}

#[test]
fn drag_edge_with_ratio_lock_centers_the_perpendicular_axis() {
    // Right edge drag with a square lock derives height and centers it on the old
    // vertical center (0.5).
    let r = NormRect::centered(0.2, 0.2);
    let out = drag_rect(r, sides(false, true, false, false), 0.9, 0.5, Some(1.0));
    assert!(approx(out.w, out.h), "not square: {out:?}");
    let cy = out.y + out.h * 0.5;
    assert!(approx(cy, 0.5), "perpendicular not centered: {out:?}");
}

#[test]
fn drag_ratio_lock_is_inert_on_a_degenerate_ratio() {
    let r = NormRect::centered(0.3, 0.3);
    let out = drag_rect(r, sides(false, true, false, true), 0.8, 0.8, Some(0.0));
    // Falls through to the free result (no NaN, no panic).
    assert!(out.w.is_finite() && out.h.is_finite());
}

// --- cache_token (disk-cache discriminator for cropped thumbnails) -----------

#[test]
fn cache_token_is_stable_and_distinguishes_edits() {
    let a = CropEdit {
        angle_deg: 2.0,
        rect: NormRect::centered(0.8, 0.8),
    };
    // Same edit → same token (stable; the disk cache relies on this across runs).
    assert_eq!(a.cache_token(), a.cache_token());
    let same = CropEdit {
        angle_deg: 2.0,
        rect: NormRect::centered(0.8, 0.8),
    };
    assert_eq!(a.cache_token(), same.cache_token());
    // A different angle or rect → a different token.
    let diff_angle = CropEdit {
        angle_deg: 2.5,
        ..a
    };
    let diff_rect = CropEdit {
        rect: NormRect::centered(0.7, 0.8),
        ..a
    };
    assert_ne!(a.cache_token(), diff_angle.cache_token());
    assert_ne!(a.cache_token(), diff_rect.cache_token());
    // Identity has its own token, distinct from any real crop.
    assert_ne!(CropEdit::identity().cache_token(), a.cache_token());
}

/// `plan_crop` must never emit a zero/oversized dimension or a window that
/// overruns the bounding box, even when handed an unsanitized edit with NaN,
/// infinite, negative, or out-of-range rect components. The output is the pixel
/// recipe the renderer and export executor both trust, so a degenerate input
/// has to degrade to a valid (clamped) recipe, never panic or produce garbage.
#[test]
fn plan_crop_clamps_degenerate_edits_to_a_valid_recipe() {
    let degenerate = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -5.0, 5.0, 0.0];
    for &x in &degenerate {
        for &w in &degenerate {
            for &angle in &[0.0f32, 12.0, f32::NAN] {
                let edit = CropEdit {
                    angle_deg: angle,
                    rect: NormRect { x, y: x, w, h: w },
                };
                let r = plan_crop(4000, 3000, &edit);
                assert!(r.bbox_w >= 1 && r.bbox_h >= 1);
                assert!(r.out_w >= 1 && r.out_w <= r.bbox_w, "out_w in range");
                assert!(r.out_h >= 1 && r.out_h <= r.bbox_h, "out_h in range");
                assert!(r.crop_x + r.out_w <= r.bbox_w, "x window inside bbox");
                assert!(r.crop_y + r.out_h <= r.bbox_h, "y window inside bbox");
            }
        }
    }
}
