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
use dcs_domain::photo::Orientation;
use dcs_domain::thumb::ThumbImage;
use image::DynamicImage;
use image::imageops::FilterType;
use turbojpeg::{Decompressor, Image, PixelFormat, ScalingFactor};

/// Decodes thumbnails asynchronously. Requests carry an opaque caller key that
/// is echoed back with the result; the caller assigns meaning (e.g. an epoch
/// to discard thumbnails from a closed folder, plus a base/hi-res tier bit).
/// Every request yields exactly one result — `None` if the decode failed — so
/// the caller can always retire its in-flight entry. Never blocks the caller.
pub trait ThumbDecoder: Send + Sync {
    /// Queue a decode at `edge` pixels (the thumbnail's longest side).
    fn request(&self, key: u64, path: PathBuf, orientation: Orientation, edge: u32);

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
    fn request(&self, key: u64, path: PathBuf, orientation: Orientation, edge: u32) {
        let tx = self.tx.clone();
        self.pool.spawn(move || {
            let _ = tx.send((key, decode_thumbnail(&path, orientation, edge)));
        });
    }

    fn poll(&self) -> Vec<(u64, Option<ThumbImage>)> {
        self.rx.try_iter().collect()
    }
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
        image::RgbaImage::from_raw(width as u32, height as u32, pixels).map(DynamicImage::ImageRgba8)
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
