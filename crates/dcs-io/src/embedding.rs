//! Local CLIP-style embeddings for AI search. A single background worker owns the
//! loaded SigLIP model (candle, CPU) and turns photos and the typed query into
//! comparable unit vectors; the pure ranking lives in `dcs-domain::search`.
//!
//! The `Embedder` trait is the seam — candle, the tokenizer, and the model files
//! never leak above `dcs-io`. Requests are queued and results polled, exactly
//! like [`crate::imaging::ThumbDecoder`]; nothing here blocks the caller. Image
//! embedding runs at low priority (a whole-folder sweep); a text query jumps the
//! queue so search stays responsive.

use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::siglip::{Config, Model};
use crossbeam_channel::{Receiver, Select, Sender, bounded, unbounded};
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::Orientation;
use image::{DynamicImage, RgbaImage};
use rayon::prelude::*;
use thiserror::Error;
use tokenizers::Tokenizer;

use crate::imaging::decode_thumbnail;

/// Identifies the model. Stored alongside each cached embedding so a future model
/// swap invalidates old vectors instead of mixing dimensions.
pub const MODEL_ID: &str = "siglip-base-patch16-384";

/// The model, baked into the binary. `build.rs` produces these files (fp16
/// weights, tokenizer, config) in `OUT_DIR` and points the env vars at them;
/// `include_bytes!` embeds them as `'static` slices — no runtime download, no disk
/// read, works fully offline.
mod embedded {
    pub static WEIGHTS: &[u8] = include_bytes!(env!("DCS_EMBED_WEIGHTS"));
    pub static TOKENIZER: &[u8] = include_bytes!(env!("DCS_EMBED_TOKENIZER"));
    pub static CONFIG: &[u8] = include_bytes!(env!("DCS_EMBED_CONFIG"));
}

/// SigLIP's text padding token id. The tokenizer pads short queries up to the
/// model's fixed sequence length with this id.
const PAD_TOKEN_ID: u32 = 1;

/// How many images to decode in parallel and embed in one batched forward. Decode
/// (rayon) overlaps with inference, and a wide forward uses the matmul far better
/// than many single-image calls — the main indexing speedup.
const IMAGE_BATCH: usize = 8;

/// Caption templates the query is wrapped in before encoding. SigLIP was trained
/// on caption-like text, so `"a photo of a temple."` matches far better than the
/// bare word; averaging several templates (the standard CLIP prompt ensemble)
/// gives a sturdier, more general query vector across subjects.
const PROMPT_TEMPLATES: [&str; 5] = [
    "a photo of a {}.",
    "a photo of {}.",
    "a close-up photo of a {}.",
    "a photograph of {}.",
    "{}.",
];

/// One image to embed, decoded fresh from the original at the model's input size
/// (the grid thumb is smaller than the 384px input, so upscaling it would lose
/// the very detail this resolution buys). `epoch` is the caller's folder epoch,
/// echoed back so a result from a closed folder can be dropped.
pub struct EmbedRequest {
    pub epoch: u64,
    pub fingerprint: ContentFingerprint,
    pub path: PathBuf,
    pub orientation: Orientation,
}

/// A finished embedding, tagged by what was embedded so the consumer can route
/// it (cache the photo vector, or resolve the query), and by the originating
/// `epoch` so stale results from a previous folder are discarded.
#[derive(Debug, Clone)]
pub enum EmbedResult {
    Image {
        epoch: u64,
        fingerprint: ContentFingerprint,
        vec: Vec<f32>,
    },
    Text {
        epoch: u64,
        query: String,
        vec: Vec<f32>,
    },
}

/// Failures loading or running the model. The worker degrades a per-item failure
/// to "no result" rather than dying; these errors are for construction only.
#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("loading model: {message}")]
    Load { message: String },
    #[error("embedding worker exited before it finished loading")]
    WorkerGone,
}

/// Embeds photos and text queries off the caller's thread. Requests are queued
/// and never block; results arrive via [`Self::poll`].
pub trait Embedder: Send + Sync {
    /// The model identity, stored with each cached vector to invalidate on swap.
    fn model_id(&self) -> &'static str;

    /// Queue an image embed (low priority — the background sweep).
    fn embed_image(&self, req: EmbedRequest);

    /// Queue a text-query embed (high priority — jumps ahead of the sweep).
    /// `epoch` is echoed on the result so a query from a closed folder is dropped.
    fn embed_text(&self, epoch: u64, query: String);

    /// Take every finished embedding since the last call. Non-blocking.
    fn poll(&self) -> Vec<EmbedResult>;
}

/// candle SigLIP embedder. Owns one worker thread that holds the loaded model;
/// the model never crosses a thread boundary, so no `Send`/lock juggling.
pub struct SiglipEmbedder {
    text_tx: Sender<(u64, String)>,
    image_tx: Sender<EmbedRequest>,
    result_rx: Receiver<EmbedResult>,
    _worker: JoinHandle<()>,
}

impl SiglipEmbedder {
    /// Load the embedded model and start the worker. Blocks until the model has
    /// loaded (or failed to), so the caller learns the outcome before reporting
    /// "ready" to the UI.
    pub fn new() -> Result<Self, EmbeddingError> {
        let (text_tx, text_rx) = unbounded::<(u64, String)>();
        let (image_tx, image_rx) = unbounded::<EmbedRequest>();
        let (result_tx, result_rx) = unbounded::<EmbedResult>();
        let (ready_tx, ready_rx) = bounded::<Result<(), EmbeddingError>>(1);

        let worker = thread::Builder::new()
            .name("dcs-embed".into())
            .spawn(move || worker_main(text_rx, image_rx, result_tx, ready_tx))
            .map_err(|e| EmbeddingError::Load {
                message: e.to_string(),
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(SiglipEmbedder {
                text_tx,
                image_tx,
                result_rx,
                _worker: worker,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(EmbeddingError::WorkerGone),
        }
    }
}

impl Embedder for SiglipEmbedder {
    fn model_id(&self) -> &'static str {
        MODEL_ID
    }

    fn embed_image(&self, req: EmbedRequest) {
        let _ = self.image_tx.send(req);
    }

    fn embed_text(&self, epoch: u64, query: String) {
        let _ = self.text_tx.send((epoch, query));
    }

    fn poll(&self) -> Vec<EmbedResult> {
        self.result_rx.try_iter().collect()
    }
}

/// The loaded model and the few constants the worker needs per inference.
struct Loaded {
    model: Model,
    tokenizer: Tokenizer,
    device: Device,
    /// Compute dtype: `F16` on GPU (≈2× faster, negligible quality loss), `F32`
    /// on CPU (where `F16` is slower) or if a GPU `F16` kernel is missing.
    dtype: DType,
    image_size: usize,
    max_len: usize,
}

/// Worker entry point: load the model (reporting success/failure on `ready`),
/// then serve text queries first and the image sweep second until both queues
/// disconnect.
fn worker_main(
    text_rx: Receiver<(u64, String)>,
    image_rx: Receiver<EmbedRequest>,
    result_tx: Sender<EmbedResult>,
    ready_tx: Sender<Result<(), EmbeddingError>>,
) {
    let loaded = match load() {
        Ok(l) => l,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    let _ = ready_tx.send(Ok(()));

    loop {
        match next_job(&text_rx, &image_rx) {
            Some(Job::Text(epoch, query)) => {
                if let Some(vec) = embed_text(&loaded, &query)
                    && result_tx
                        .send(EmbedResult::Text { epoch, query, vec })
                        .is_err()
                {
                    return;
                }
            }
            Some(Job::Image(first)) => {
                // Gather everything already queued (up to a batch) so decode runs
                // in parallel and the model sees one wide forward instead of many.
                let mut batch = vec![first];
                while batch.len() < IMAGE_BATCH {
                    match image_rx.try_recv() {
                        Ok(req) => batch.push(req),
                        Err(_) => break,
                    }
                }
                for result in embed_image_batch(&loaded, &batch) {
                    if result_tx.send(result).is_err() {
                        return;
                    }
                }
            }
            None => return, // both queues closed — embedder dropped
        }
    }
}

enum Job {
    Text(u64, String),
    Image(EmbedRequest),
}

fn text_job((epoch, query): (u64, String)) -> Job {
    Job::Text(epoch, query)
}

/// The next job, text queue first, blocking when both are empty. Exits (`None`)
/// only when both queues have disconnected.
fn next_job(text_rx: &Receiver<(u64, String)>, image_rx: &Receiver<EmbedRequest>) -> Option<Job> {
    use crossbeam_channel::TryRecvError;
    match text_rx.try_recv() {
        Ok(t) => return Some(text_job(t)),
        Err(TryRecvError::Disconnected) => return image_rx.recv().ok().map(Job::Image),
        Err(TryRecvError::Empty) => {}
    }
    match image_rx.try_recv() {
        Ok(req) => return Some(Job::Image(req)),
        Err(TryRecvError::Disconnected) => return text_rx.recv().ok().map(text_job),
        Err(TryRecvError::Empty) => {}
    }
    let mut sel = Select::new();
    let text_op = sel.recv(text_rx);
    let image_op = sel.recv(image_rx);
    let op = sel.select();
    if op.index() == text_op {
        op.recv(text_rx).ok().map(text_job)
    } else {
        debug_assert_eq!(op.index(), image_op);
        op.recv(image_rx).ok().map(Job::Image)
    }
}

/// Load the SigLIP architecture, weights, and tokenizer from the embedded bytes.
/// Reading the config rather than hardcoding a preset lets the same code load any
/// SigLIP variant.
fn load() -> Result<Loaded, EmbeddingError> {
    let err = |message: String| EmbeddingError::Load { message };
    let (device, backend) = best_device();

    let config: Config =
        serde_json::from_slice(embedded::CONFIG).map_err(|e| err(e.to_string()))?;
    let tokenizer = Tokenizer::from_bytes(embedded::TOKENIZER).map_err(|e| err(e.to_string()))?;
    let image_size = config.vision_config.image_size;

    // GPU: prefer F16 (≈2× the throughput); probe a dummy forward and fall back to
    // F32 if a kernel is missing. CPU: F32 always (F16 is slower there).
    let (model, dtype) = load_model(&config, &device, backend != "cpu")?;
    eprintln!("dcs: embedding on {backend}, {dtype:?}");

    Ok(Loaded {
        model,
        tokenizer,
        device,
        dtype,
        image_size,
        max_len: config.text_config.max_position_embeddings,
    })
}

/// Build the model, preferring `F16` on GPU. The dtype is validated with a dummy
/// vision forward (which also warms kernels); if `F16` fails, retry in `F32`.
fn load_model(
    config: &Config,
    device: &Device,
    prefer_f16: bool,
) -> Result<(Model, DType), EmbeddingError> {
    if prefer_f16 && let Ok(model) = build_and_probe(config, device, DType::F16) {
        return Ok((model, DType::F16));
    }
    let model = build_and_probe(config, device, DType::F32).map_err(|e| EmbeddingError::Load {
        message: e.to_string(),
    })?;
    Ok((model, DType::F32))
}

/// Load the weights at `dtype` and run one throwaway forward so a missing-kernel
/// failure surfaces here (where we can fall back) rather than silently dropping
/// every embed batch later. The embedded slice is `'static`, so no copy and no
/// file to outlive.
fn build_and_probe(config: &Config, device: &Device, dtype: DType) -> candle_core::Result<Model> {
    let vb = VarBuilder::from_slice_safetensors(embedded::WEIGHTS, dtype, device)?;
    let model = Model::new(config, vb)?;
    let size = config.vision_config.image_size;
    let dummy = Tensor::zeros((1, 3, size, size), dtype, device)?;
    model.get_image_features(&dummy)?;
    Ok(model)
}

/// The best inference device available, with the matching label. Prefers CUDA,
/// then Metal, then CPU. Each backend is compiled in only when its feature is
/// enabled (candle gates `new_cuda`/`new_metal` the same way), so this falls back
/// cleanly when a GPU is unavailable or its toolkit/driver isn't present.
fn best_device() -> (Device, &'static str) {
    #[cfg(feature = "cuda")]
    if let Ok(device) = Device::new_cuda(0) {
        return (device, "cuda (gpu)");
    }
    #[cfg(feature = "metal")]
    if let Ok(device) = Device::new_metal(0) {
        return (device, "metal (gpu)");
    }
    (Device::Cpu, "cpu")
}

/// Embed a batch of images in one forward. Decode + resize runs in parallel on
/// the CPU (rayon, no GPU calls off the worker thread); the pixels are then
/// uploaded as a single tensor and run through the vision tower once. Per-image
/// vectors are identical to embedding them singly. A decode failure drops just
/// that image; an inference failure drops the whole batch (those photos stay
/// uncached and are retried on the next open).
fn embed_image_batch(loaded: &Loaded, batch: &[EmbedRequest]) -> Vec<EmbedResult> {
    let size = loaded.image_size;
    let prepared: Vec<(u64, ContentFingerprint, Vec<u8>)> = batch
        .par_iter()
        .filter_map(|req| Some((req.epoch, req.fingerprint, decode_rgb(req, size)?)))
        .collect();
    if prepared.is_empty() {
        return Vec::new();
    }
    let Ok(pixels) = batch_tensor(loaded, prepared.iter().map(|(_, _, rgb)| rgb.as_slice())) else {
        return Vec::new();
    };
    let Ok(features) = loaded.model.get_image_features(&pixels) else {
        return Vec::new();
    };
    prepared
        .iter()
        .enumerate()
        .filter_map(|(i, (epoch, fingerprint, _))| {
            let row = features.get(i).ok()?;
            let vec = unit_vec(&row)?;
            Some(EmbedResult::Image {
                epoch: *epoch,
                fingerprint: *fingerprint,
                vec,
            })
        })
        .collect()
}

/// Embed a text query to a unit vector comparable to image vectors. The query is
/// expanded through [`PROMPT_TEMPLATES`] and the per-template vectors are averaged
/// (then renormalized) — a sturdier query than encoding the bare word once.
fn embed_text(loaded: &Loaded, query: &str) -> Option<Vec<f32>> {
    let rows: Vec<Vec<u32>> = PROMPT_TEMPLATES
        .iter()
        .map(|t| token_ids(loaded, &t.replace("{}", query)))
        .collect::<Option<_>>()?;
    let n = rows.len();
    let flat: Vec<u32> = rows.into_iter().flatten().collect();
    // One batched forward over every template: (n_templates, seq) → (n_templates, dim).
    let input = Tensor::from_vec(flat, (n, loaded.max_len), &loaded.device).ok()?;
    let features = loaded.model.get_text_features(&input).ok()?;
    let mean = features.mean(0).ok()?;
    unit_vec(&mean)
}

/// Decode one image and squash-resize it to the model square, returning raw RGB
/// bytes (`size*size*3`). SigLIP's processor resizes **anisotropically** to
/// `size×size` (no center crop), so we use `resize_exact` to match the training
/// distribution rather than `resize_to_fill` (which would crop the edges). Pure
/// CPU and free of candle/device calls, so it's safe across rayon threads even on
/// a GPU device. `None` on decode failure.
fn decode_rgb(req: &EmbedRequest, size: usize) -> Option<Vec<u8>> {
    let thumb = decode_thumbnail(&req.path, req.orientation, size as u32, None)?;
    let img = RgbaImage::from_raw(thumb.width, thumb.height, thumb.rgba)?;
    Some(
        DynamicImage::ImageRgba8(img)
            .resize_exact(
                size as u32,
                size as u32,
                image::imageops::FilterType::Triangle,
            )
            .to_rgb8()
            .into_raw(),
    )
}

/// Stack per-image RGB bytes into one SigLIP input tensor on the model's device:
/// `(B, 3, size, size)`, scaled to `[-1, 1]`. The single upload/build runs on the
/// worker thread, keeping device work off the rayon decode threads.
fn batch_tensor<'a>(
    loaded: &Loaded,
    rows: impl Iterator<Item = &'a [u8]>,
) -> candle_core::Result<Tensor> {
    let size = loaded.image_size;
    let mut flat: Vec<u8> = Vec::new();
    let mut count = 0usize;
    for row in rows {
        flat.extend_from_slice(row);
        count += 1;
    }
    Tensor::from_vec(flat, (count, size, size, 3), &loaded.device)?
        .permute((0, 3, 1, 2))?
        .to_dtype(DType::F32)?
        .affine(2.0 / 255.0, -1.0)?
        // Match the model's compute dtype (F16 on GPU). The scale/shift stays in
        // F32 for precision, then casts.
        .to_dtype(loaded.dtype)
}

/// Tokenize a query and pad/truncate to the model's fixed sequence length.
fn token_ids(loaded: &Loaded, query: &str) -> Option<Vec<u32>> {
    let encoding = loaded.tokenizer.encode(query, true).ok()?;
    let mut ids = encoding.get_ids().to_vec();
    ids.truncate(loaded.max_len);
    ids.resize(loaded.max_len, PAD_TOKEN_ID);
    Some(ids)
}

/// Flatten a `(1, dim)` feature tensor and L2-normalize it, so cosine similarity
/// downstream is a plain dot product. Casts to `F32` first so an `F16` (GPU)
/// result reads out cleanly. Returns `None` if the tensor won't read out.
fn unit_vec(features: &Tensor) -> Option<Vec<f32>> {
    let raw: Vec<f32> = features
        .to_dtype(DType::F32)
        .ok()?
        .flatten_all()
        .ok()?
        .to_vec1()
        .ok()?;
    let norm = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return Some(raw);
    }
    Some(raw.iter().map(|x| x / norm).collect())
}
