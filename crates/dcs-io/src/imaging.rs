//! Thumbnail decoding. A fixed pool of worker threads decodes JPEGs off the UI
//! thread with libjpeg-turbo (SIMD), scaled down on decode so a 24 MP frame
//! costs a few milliseconds, bakes in EXIF orientation, and contain-fits to a
//! square box. Decoding the JPEG itself — not the camera's letterboxed EXIF
//! thumbnail — means the result is the real image: correct aspect, no bars.
//!
//! The `ThumbDecoder` trait is the seam: each request decodes at a target pixel
//! edge (the caller sizes it to the cell) and echoes back an opaque key. Two
//! priority queues feed the workers (see [`ThumbDecoderPool`]).

use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread::{self, JoinHandle, available_parallelism};

use crossbeam_channel::{Receiver, Select, Sender, unbounded};
use dcs_domain::crops::{CropEdit, SourceSampler, plan_crop};
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::Orientation;
use dcs_domain::thumb::ThumbImage;
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};
use rayon::prelude::*;
use turbojpeg::{Decompressor, Image, PixelFormat, ScalingFactor, Subsamp};

use crate::cache::{SharedCache, ThumbCache, ThumbTier};

/// Hard ceiling on the source decode edge when a crop needs extra resolution, so
/// a tiny crop window can't ask for an unbounded decode.
const MAX_DECODE_EDGE: u32 = 8192;

/// JPEG quality for cached thumbnail blobs. High enough that the cache is
/// visually indistinguishable from a fresh decode, small enough that a 5–6k
/// folder's grid tier fits the disk budget comfortably.
const CACHE_JPEG_QUALITY: i32 = 88;

/// Scheduling lane for a decode. `High` is for what the user is looking at right
/// now (the visible grid band, a zoomed cell, the gallery frame and filmstrip);
/// `Low` is the whole-folder background prefetch. The two run on separate worker
/// pools so a backlog of background thumbnails never delays a foreground decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodePriority {
    High,
    Low,
}

/// One decode job. Beyond the source file, it carries an optional disk-cache
/// identity: when `cache_key` and `cache` are both set, the worker serves the
/// thumbnail from the cache on a hit and populates it on a miss — all on the
/// decode thread, so the UI thread never touches SQLite or the JPEG codec.
pub struct DecodeRequest {
    /// Opaque caller key, echoed back with the result.
    pub key: u64,
    pub path: PathBuf,
    pub orientation: Orientation,
    /// Target pixel edge (the thumbnail's longest side).
    pub edge: u32,
    /// Content identity for the disk cache; `None` skips the cache entirely.
    pub cache_key: Option<ContentFingerprint>,
    pub tier: ThumbTier,
    pub cache: Option<SharedCache>,
    /// Which worker pool runs this decode.
    pub priority: DecodePriority,
    /// The committed crop+straighten to bake into the decoded pixels, if any.
    /// Applied after orientation, before the fit-resize. When `cache_key` is also
    /// set the result is disk-cached under a key that folds the crop, so each
    /// distinct crop gets its own entry.
    pub crop: Option<CropEdit>,
}

/// Decodes thumbnails asynchronously. Requests carry an opaque caller key that
/// is echoed back with the result; the caller assigns meaning (e.g. an epoch
/// to discard thumbnails from a closed folder, plus a base/hi-res tier bit).
/// Every request yields exactly one result — `None` if the decode failed — so
/// the caller can always retire its in-flight entry. Never blocks the caller.
pub trait ThumbDecoder: Send + Sync {
    /// Queue a decode, optionally backed by the disk cache (see `DecodeRequest`).
    fn request(&self, req: DecodeRequest);

    /// Take every result produced since the last call. Non-blocking.
    fn poll(&self) -> Vec<(u64, Option<ThumbImage>)>;
}

/// A fixed set of worker threads (one per core) draining two queues: every
/// worker takes from the high queue first and only falls back to the low queue
/// when high is empty. So the whole machine chews through a first-load backlog
/// (all of it low priority) at full width, yet the moment a foreground decode
/// (gallery frame, zoomed cell) is queued, the next free worker picks it ahead
/// of the remaining background thumbnails. No queue is starved; none is
/// pre-allocated a fraction of the cores.
pub struct ThumbDecoderPool {
    high_tx: Sender<DecodeRequest>,
    low_tx: Sender<DecodeRequest>,
    result_rx: Receiver<(u64, Option<ThumbImage>)>,
    // Held so the worker threads are joined-or-detached with the pool; dropping
    // the senders disconnects the queues and the workers exit on their own.
    _workers: Vec<JoinHandle<()>>,
}

impl ThumbDecoderPool {
    pub fn new() -> Self {
        let cores = available_parallelism().map(|n| n.get()).unwrap_or(4);
        let (high_tx, high_rx) = unbounded::<DecodeRequest>();
        let (low_tx, low_rx) = unbounded::<DecodeRequest>();
        let (result_tx, result_rx) = unbounded();
        let workers = (0..cores)
            .map(|i| {
                let high_rx = high_rx.clone();
                let low_rx = low_rx.clone();
                let result_tx = result_tx.clone();
                thread::Builder::new()
                    .name(format!("dcs-decode-{i}"))
                    .spawn(move || decode_worker(high_rx, low_rx, result_tx))
                    .expect("spawning a decode worker thread")
            })
            .collect();
        ThumbDecoderPool {
            high_tx,
            low_tx,
            result_rx,
            _workers: workers,
        }
    }
}

/// One worker: prefer the high queue, fall back to low, otherwise block until
/// either delivers. Exits when both queues disconnect (the pool was dropped).
fn decode_worker(
    high_rx: Receiver<DecodeRequest>,
    low_rx: Receiver<DecodeRequest>,
    result_tx: Sender<(u64, Option<ThumbImage>)>,
) {
    loop {
        let req = match next_request(&high_rx, &low_rx) {
            Some(req) => req,
            None => return, // both queues closed — pool dropped
        };
        let key = req.key;
        if result_tx.send((key, decode_with_cache(req))).is_err() {
            return; // results no longer wanted
        }
    }
}

/// The next request to run, high queue first, blocking when both are empty.
fn next_request(
    high_rx: &Receiver<DecodeRequest>,
    low_rx: &Receiver<DecodeRequest>,
) -> Option<DecodeRequest> {
    use crossbeam_channel::TryRecvError;
    match high_rx.try_recv() {
        Ok(req) => return Some(req),
        Err(TryRecvError::Disconnected) => {
            // High closed: low alone until it closes too.
            return low_rx.recv().ok();
        }
        Err(TryRecvError::Empty) => {}
    }
    match low_rx.try_recv() {
        Ok(req) => return Some(req),
        Err(TryRecvError::Disconnected) => return high_rx.recv().ok(),
        Err(TryRecvError::Empty) => {}
    }
    // Both empty: block until either is ready, biased to re-check high first.
    let mut sel = Select::new();
    let high_op = sel.recv(high_rx);
    let low_op = sel.recv(low_rx);
    let op = sel.select();
    if op.index() == high_op {
        op.recv(high_rx).ok().or_else(|| low_rx.recv().ok())
    } else {
        debug_assert_eq!(op.index(), low_op);
        op.recv(low_rx).ok().or_else(|| high_rx.recv().ok())
    }
}

impl Default for ThumbDecoderPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbDecoder for ThumbDecoderPool {
    fn request(&self, req: DecodeRequest) {
        let queue = match req.priority {
            DecodePriority::High => &self.high_tx,
            DecodePriority::Low => &self.low_tx,
        };
        // A closed receiver only happens after the workers are gone (shutdown).
        let _ = queue.send(req);
    }

    fn poll(&self) -> Vec<(u64, Option<ThumbImage>)> {
        self.result_rx.try_iter().collect()
    }
}

/// Resolve a thumbnail, going through the disk cache when the request carries an
/// identity. A cache hit decodes the stored JPEG blob (already oriented, cropped,
/// and sized); a miss decodes the original, then encodes and stores it. The cache
/// lock is taken only for the keyed get/put — the JPEG encode runs off-lock.
///
/// A cropped decode bakes a per-photo edit into the pixels; it still caches, under
/// a content key that folds the crop ([`crop_cache_key`]) so each distinct crop
/// gets its own entry and an unedited reopen of the folder paints the cropped
/// thumbnail straight from disk.
fn decode_with_cache(req: DecodeRequest) -> Option<ThumbImage> {
    let key = req.cache_key.map(|fp| match req.crop.as_ref() {
        Some(crop) => crop_cache_key(&fp, crop),
        None => fp,
    });

    if let (Some(key), Some(cache)) = (key, req.cache.as_ref()) {
        let cached = cache
            .lock()
            .ok()
            .and_then(|guard| guard.get(&key, req.tier));
        if let Some(blob) = cached
            && let Some(thumb) = decode_blob(&blob)
        {
            return Some(thumb);
        }
    }

    let thumb = decode_thumbnail(&req.path, req.orientation, req.edge, req.crop.as_ref())?;

    if let (Some(key), Some(cache)) = (key, req.cache.as_ref())
        && let Some(blob) = encode_blob(&thumb)
        && let Ok(guard) = cache.lock()
    {
        guard.put(&key, req.tier, &blob);
    }
    Some(thumb)
}

/// Fold a crop into a content fingerprint so a cropped thumbnail keys its own disk
/// cache entry, distinct from the uncropped one and from other crops. Stable
/// across runs (blake3 over the fingerprint + the edit's [`CropEdit::cache_token`]).
fn crop_cache_key(fp: &ContentFingerprint, crop: &CropEdit) -> ContentFingerprint {
    let mut hasher = blake3::Hasher::new();
    hasher.update(fp.as_bytes());
    hasher.update(&crop.cache_token().to_le_bytes());
    ContentFingerprint::from_bytes(*hasher.finalize().as_bytes())
}

/// Encode an already-prepared RGBA thumbnail to a JPEG blob for the cache.
/// Returns `None` if the codec rejects the buffer (never fatal — a failed
/// encode just means this thumb isn't cached).
fn encode_blob(thumb: &ThumbImage) -> Option<Vec<u8>> {
    if thumb.width == 0 || thumb.height == 0 {
        return None;
    }
    let image = Image {
        pixels: thumb.rgba.as_slice(),
        width: thumb.width as usize,
        pitch: thumb.width as usize * 4,
        height: thumb.height as usize,
        format: PixelFormat::RGBA,
    };
    turbojpeg::compress(image, CACHE_JPEG_QUALITY, Subsamp::Sub2x2)
        .ok()
        .map(|buf| buf.to_vec())
}

/// Decode a cached JPEG blob back to RGBA. The blob was stored post-orientation
/// and post-resize, so this is a straight decompress — no further transforms.
fn decode_blob(blob: &[u8]) -> Option<ThumbImage> {
    let image = turbojpeg::decompress(blob, PixelFormat::RGBA).ok()?;
    Some(ThumbImage {
        width: image.width as u32,
        height: image.height as u32,
        rgba: image.pixels,
    })
}

/// Decode one thumbnail at the given pixel edge: libjpeg-turbo DCT-scaled
/// decode, orientation baked in, optional straighten+crop, contain-fit to an
/// `edge × edge` box. Returns `None` for anything turbojpeg and the `image`
/// fallback both reject (RAW, corrupt, missing).
///
/// When `crop` is set, the source is decoded at higher resolution (so the crop
/// window still resolves to ~`edge` px) before the transform.
pub fn decode_thumbnail(
    path: &Path,
    orientation: Orientation,
    edge: u32,
    crop: Option<&CropEdit>,
) -> Option<ThumbImage> {
    let decode_edge = crop.map_or(edge, |e| crop_decode_edge(e, edge));
    let img = decode_scaled(path, decode_edge).or_else(|| image::open(path).ok())?;
    let img = apply_orientation(img, orientation);
    let img = match crop {
        Some(edit) => apply_crop(&img.into_rgba8(), edit, edge),
        None => img,
    };
    Some(finish(img, edge))
}

/// Decode a JPEG at full resolution with EXIF orientation baked into the pixels,
/// as an `RgbaImage`. For the export render path, which needs the original
/// resolution (no thumbnail downscale). `None` if the file can't be decoded.
pub fn decode_oriented_full(path: &Path, orientation: Orientation) -> Option<RgbaImage> {
    let img = decode_scaled(path, u32::MAX).or_else(|| image::open(path).ok())?;
    Some(apply_orientation(img, orientation).into_rgba8())
}

/// Encode an RGBA image to a JPEG blob at the given quality. `None` if the codec
/// rejects the buffer.
pub fn encode_jpeg(img: &RgbaImage, quality: i32) -> Option<Vec<u8>> {
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    let image = Image {
        pixels: img.as_raw().as_slice(),
        width: img.width() as usize,
        pitch: img.width() as usize * 4,
        height: img.height() as usize,
        format: PixelFormat::RGBA,
    };
    turbojpeg::compress(image, quality, Subsamp::Sub2x2)
        .ok()
        .map(|buf| buf.to_vec())
}

/// The source decode edge for a cropped thumbnail: scaled up by the inverse of
/// the crop window's smaller normalized side, so the cropped region keeps ~`edge`
/// resolution. Capped at [`MAX_DECODE_EDGE`].
fn crop_decode_edge(edit: &CropEdit, edge: u32) -> u32 {
    let min_side = edit.rect.w.min(edit.rect.h).max(0.05);
    let scaled = (edge as f32 / min_side).ceil() as u32;
    // `edge` can exceed the cap (a future caller / very large request); clamp the
    // low bound too so the range can't invert and panic.
    scaled.clamp(edge.min(MAX_DECODE_EDGE), MAX_DECODE_EDGE)
}

/// Contain-fit into an `edge × edge` box, never upscaling beyond it. The input is
/// already RGBA, so `into_rgba8` moves rather than converts.
fn finish(img: DynamicImage, edge: u32) -> ThumbImage {
    let resized = if img.width() > edge || img.height() > edge {
        img.resize(edge, edge, FilterType::Triangle)
    } else {
        img
    };
    let rgba = resized.into_rgba8();
    ThumbImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    }
}

/// Bake a straighten+crop into an oriented RGBA image. One bilinear resample
/// handles rotation, crop, and downscaling together: each output pixel maps back
/// through the inverse straighten rotation to a source point and is sampled.
/// Output size is the crop window's pixel dims, scaled so its longest side is at
/// most `edge` (no upscale). Pure pixel work — the geometry comes from
/// [`plan_crop`], the single shared crop math path.
pub fn apply_crop(src: &RgbaImage, edit: &CropEdit, edge: u32) -> DynamicImage {
    let (sw, sh) = (src.width(), src.height());
    let plan = plan_crop(sw, sh, edit);
    // Output dims: the crop window, capped so the long side is ≤ edge.
    let long = plan.out_w.max(plan.out_h).max(1);
    let scale = (edge as f32 / long as f32).min(1.0);
    let ow = ((plan.out_w as f32 * scale).round() as u32).max(1);
    let oh = ((plan.out_h as f32 * scale).round() as u32).max(1);

    // The straighten+crop geometry is the pure domain transform; this only walks
    // output pixels and bilinear-fetches the source coordinate it returns. Rows
    // are independent, so a full-res export (tens of millions of pixels) fans out
    // across the rayon pool; the sampler is `Copy`/`Send` and the source is `Sync`.
    let sampler = SourceSampler::new(sw, sh, edit.angle_deg);
    let rect = edit.rect;
    let raw = src.as_raw().as_slice();
    let (sw_i, sh_i) = (sw as i32, sh as i32);
    let row_bytes = ow as usize * 4;
    let mut buf = vec![0u8; row_bytes * oh as usize];
    buf.par_chunks_mut(row_bytes)
        .enumerate()
        .for_each(|(oy, row)| {
            let ny = rect.y + (oy as f32 + 0.5) / oh as f32 * rect.h;
            for ox in 0..ow as usize {
                let nx = rect.x + (ox as f32 + 0.5) / ow as f32 * rect.w;
                let (sx, sy) = sampler.source_at(nx, ny);
                let px = sample_bilinear(raw, sw_i, sh_i, sx, sy);
                row[ox * 4..ox * 4 + 4].copy_from_slice(&px);
            }
        });
    // SAFETY-of-invariant: `buf` is exactly `ow * oh * 4` bytes, so `from_raw`
    // cannot fail.
    let img = RgbaImage::from_raw(ow, oh, buf).expect("buffer is ow*oh*4 RGBA bytes");
    DynamicImage::ImageRgba8(img)
}

/// Bilinear-sample an RGBA `raw` buffer (`w`×`h`) at fractional `(x, y)`, clamping
/// to the image edge so an off-image coordinate (rounding at the border) yields
/// the nearest edge pixel rather than a transparent hole. Indexes the slice
/// directly — no per-pixel `GenericImage` dispatch on the hot path.
fn sample_bilinear(raw: &[u8], w: i32, h: i32, x: f32, y: f32) -> [u8; 4] {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let at = |ix: i32, iy: i32| -> [f32; 4] {
        let cx = ix.clamp(0, w - 1);
        let cy = iy.clamp(0, h - 1);
        let o = ((cy * w + cx) * 4) as usize;
        [
            raw[o] as f32,
            raw[o + 1] as f32,
            raw[o + 2] as f32,
            raw[o + 3] as f32,
        ]
    };
    let p00 = at(x0, y0);
    let p10 = at(x0 + 1, y0);
    let p01 = at(x0, y0 + 1);
    let p11 = at(x0 + 1, y0 + 1);
    std::array::from_fn(|c| {
        let top = p00[c] + (p10[c] - p00[c]) * fx;
        let bot = p01[c] + (p11[c] - p01[c]) * fx;
        (top + (bot - top) * fy).round().clamp(0.0, 255.0) as u8
    })
}

thread_local! {
    /// One libjpeg-turbo handle per decode worker, reused across images so each
    /// decode skips handle init.
    static DECOMPRESSOR: RefCell<Option<Decompressor>> = const { RefCell::new(None) };
}

/// Decode a JPEG with libjpeg-turbo, scaled down to roughly `edge` on its
/// longest side. The DCT scaling makes a full-resolution frame cheap to read.
fn decode_scaled(path: &Path, edge: u32) -> Option<DynamicImage> {
    let data = std::fs::read(path).ok()?;
    DECOMPRESSOR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(Decompressor::new().ok()?);
        }
        let decompressor = slot.as_mut()?;

        let header = decompressor.read_header(&data).ok()?;
        let factor = pick_scaling(header.width, header.height, edge as usize);
        decompressor.set_scaling_factor(factor).ok()?;
        let width = factor.scale(header.width);
        let height = factor.scale(header.height);
        if width == 0 || height == 0 {
            return None;
        }

        let mut pixels = vec![0u8; width * height * 4];
        let image = Image {
            pixels: pixels.as_mut_slice(),
            width,
            pitch: width * 4,
            height,
            format: PixelFormat::RGBA,
        };
        decompressor.decompress(&data, image).ok()?;
        image::RgbaImage::from_raw(width as u32, height as u32, pixels)
            .map(DynamicImage::ImageRgba8)
    })
}

/// Smallest supported scaling factor whose scaled longest side still covers
/// `edge`. Falls back to 1:1 for images already at or below the target. The
/// factor list is queried from libjpeg-turbo once and cached.
fn pick_scaling(width: usize, height: usize, edge: usize) -> ScalingFactor {
    let long = width.max(height);
    if long <= edge {
        return ScalingFactor::ONE;
    }
    let mut best = ScalingFactor::ONE;
    let mut best_long = long;
    for &factor in scaling_factors() {
        let scaled = factor.scale(long);
        if scaled >= edge && scaled <= best_long {
            best_long = scaled;
            best = factor;
        }
    }
    best
}

fn scaling_factors() -> &'static [ScalingFactor] {
    static FACTORS: OnceLock<Vec<ScalingFactor>> = OnceLock::new();
    FACTORS.get_or_init(Decompressor::supported_scaling_factors)
}

fn apply_orientation(mut img: DynamicImage, orientation: Orientation) -> DynamicImage {
    img.apply_orientation(to_image_orientation(orientation));
    img
}

fn to_image_orientation(orientation: Orientation) -> image::metadata::Orientation {
    use image::metadata::Orientation as I;
    match orientation {
        Orientation::Normal => I::NoTransforms,
        Orientation::FlipH => I::FlipHorizontal,
        Orientation::Rotate180 => I::Rotate180,
        Orientation::FlipV => I::FlipVertical,
        Orientation::Transpose => I::Rotate90FlipH,
        Orientation::Rotate90 => I::Rotate90,
        Orientation::Transverse => I::Rotate270FlipH,
        Orientation::Rotate270 => I::Rotate270,
    }
}
