//! Thumbnail decoding. A rayon pool decodes JPEGs off the UI thread with
//! libjpeg-turbo (SIMD), scaled down on decode so a 24 MP frame costs a few
//! milliseconds, bakes in EXIF orientation, and contain-fits to a square box
//! (§10). Decoding the JPEG itself — not the camera's letterboxed EXIF
//! thumbnail — means the result is the real image: correct aspect, no bars.
//!
//! The `ThumbDecoder` trait is the seam: each request decodes at a target pixel
//! edge (the caller sizes it to the cell) and echoes back an opaque key.

use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread::available_parallelism;

use crossbeam_channel::{Receiver, Sender, unbounded};
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::Orientation;
use dcs_domain::thumb::ThumbImage;
use image::DynamicImage;
use image::imageops::FilterType;
use turbojpeg::{Decompressor, Image, PixelFormat, ScalingFactor, Subsamp};

use crate::cache::{SharedCache, ThumbCache, ThumbTier};

/// JPEG quality for cached thumbnail blobs. High enough that the cache is
/// visually indistinguishable from a fresh decode, small enough that a 5–6k
/// folder's grid tier fits the disk budget comfortably.
const CACHE_JPEG_QUALITY: i32 = 88;

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

/// rayon-backed decoder sized to the machine's parallelism.
pub struct RayonThumbDecoder {
    pool: rayon::ThreadPool,
    tx: Sender<(u64, Option<ThumbImage>)>,
    rx: Receiver<(u64, Option<ThumbImage>)>,
}

impl RayonThumbDecoder {
    pub fn new() -> Self {
        let threads = available_parallelism().map(|n| n.get()).unwrap_or(4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("dcs-decode-{i}"))
            .build()
            .expect("rayon pool with a valid thread count always builds");
        let (tx, rx) = unbounded();
        RayonThumbDecoder { pool, tx, rx }
    }
}

impl Default for RayonThumbDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbDecoder for RayonThumbDecoder {
    fn request(&self, req: DecodeRequest) {
        let tx = self.tx.clone();
        self.pool.spawn(move || {
            let key = req.key;
            let _ = tx.send((key, decode_with_cache(req)));
        });
    }

    fn poll(&self) -> Vec<(u64, Option<ThumbImage>)> {
        self.rx.try_iter().collect()
    }
}

/// Resolve a thumbnail, going through the disk cache when the request carries an
/// identity. A cache hit decodes the stored JPEG blob (already oriented and
/// sized); a miss decodes the original, then encodes and stores it. The cache
/// lock is taken only for the keyed get/put — the JPEG encode runs off-lock.
fn decode_with_cache(req: DecodeRequest) -> Option<ThumbImage> {
    if let (Some(fp), Some(cache)) = (req.cache_key, req.cache.as_ref()) {
        let cached = cache.lock().ok().and_then(|guard| guard.get(&fp, req.tier));
        if let Some(blob) = cached
            && let Some(thumb) = decode_blob(&blob)
        {
            return Some(thumb);
        }
    }

    let thumb = decode_thumbnail(&req.path, req.orientation, req.edge)?;

    if let (Some(fp), Some(cache)) = (req.cache_key, req.cache.as_ref())
        && let Some(blob) = encode_blob(&thumb)
        && let Ok(guard) = cache.lock()
    {
        guard.put(&fp, req.tier, &blob);
    }
    Some(thumb)
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
/// decode, orientation baked in, contain-fit to an `edge × edge` box. Returns
/// `None` for anything turbojpeg and the `image` fallback both reject (RAW,
/// corrupt, missing).
pub fn decode_thumbnail(path: &Path, orientation: Orientation, edge: u32) -> Option<ThumbImage> {
    let img = decode_scaled(path, edge).or_else(|| image::open(path).ok())?;
    Some(finish(img, orientation, edge))
}

/// Apply orientation and contain-fit into an `edge × edge` box, never upscaling
/// beyond it. The input is already RGBA, so `into_rgba8` moves rather than
/// converts.
fn finish(img: DynamicImage, orientation: Orientation, edge: u32) -> ThumbImage {
    let img = apply_orientation(img, orientation);
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
        let decompressor = slot.as_mut().expect("just initialized");

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
