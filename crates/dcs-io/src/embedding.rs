//! Local CLIP-style embeddings for AI search. A single background worker owns the
//! loaded SigLIP model (ONNX Runtime) and turns photos and the typed query into
//! comparable unit vectors; the pure ranking lives in `dcs-domain::search`.
//!
//! The `Embedder` trait is the seam — ONNX Runtime, the tokenizer, and the model
//! files never leak above `dcs-io`. Requests are queued and results polled,
//! exactly like [`crate::imaging::ThumbDecoder`]; nothing here blocks the caller.
//! Image embedding runs at low priority (a whole-folder sweep); a text query
//! jumps the queue so search stays responsive.
//!
//! Inference runs on the `webgpu` execution provider (Dawn: Metal / DX12 /
//! Vulkan) — GPU acceleration on every vendor and OS with no user-installed
//! dependencies — falling back to the CPU provider when no usable GPU exists.
//! Built without the `ai-search` feature, [`SiglipEmbedder::new`] reports that
//! search is not included in this build and nothing below is compiled.

use std::path::PathBuf;

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::Orientation;
use thiserror::Error;

/// Identifies the model. Stored alongside each cached embedding so vectors from
/// a different numerical graph of the same architecture are never mixed —
/// near-identical vectors in one index skew cosine ranking.
pub const MODEL_ID: &str = "siglip-base-patch16-384-onnx";

/// One image to embed, decoded fresh from the original (the grid thumb is
/// smaller than the model input, so upscaling it would lose the very detail
/// this resolution buys). `epoch` is the caller's folder epoch, echoed back so
/// a result from a closed folder can be dropped.
pub struct EmbedRequest {
    pub epoch: u64,
    pub fingerprint: ContentFingerprint,
    pub path: PathBuf,
    pub orientation: Orientation,
}

/// A finished embedding, tagged by what was embedded so the consumer can route
/// it (cache the photo vector, resolve the query, or clear a query that could
/// not embed), and by the originating `epoch` so stale results from a previous
/// folder are discarded.
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
    /// The query could not be embedded (tokenizer or inference failure). The
    /// consumer must clear the query's pending state — without this, a failed
    /// query would show "searching…" forever.
    TextFailed { epoch: u64, query: String },
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

    /// Tell the embedder which folder epoch is current so queued work from
    /// earlier epochs can be skipped instead of decoded and inferred for
    /// nothing. Default: no-op (mocks, stubs).
    fn set_epoch(&self, _epoch: u64) {}

    /// Whether the background worker is still running. `false` means it died
    /// (a panic or an unrecoverable inference failure) and no further results
    /// will ever arrive — the caller should surface an error instead of
    /// waiting. Default: always alive (mocks, stubs).
    fn is_alive(&self) -> bool {
        true
    }
}

#[cfg(feature = "ai-search")]
pub use siglip::SiglipEmbedder;

#[cfg(not(feature = "ai-search"))]
pub use stub::SiglipEmbedder;

/// L2-normalize a feature row so cosine similarity downstream is a plain dot
/// product. A zero or non-finite norm returns the row unchanged (callers drop
/// non-finite rows before caching). Public so integration tests can pin the
/// math; not part of the conceptual API.
pub fn unit_vec(raw: &[f32]) -> Vec<f32> {
    let norm = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !(norm.is_finite() && norm > 0.0) {
        return raw.to_vec();
    }
    raw.iter().map(|x| x / norm).collect()
}

/// Flatten per-image HWC RGB bytes into one NCHW `f32` batch scaled to `[-1, 1]`
/// (SigLIP's input normalization). `None` if any image is not exactly
/// `size * size * 3` bytes — a silent mis-sized image would shift every later
/// image in the batch onto the wrong vector. Public so integration tests can
/// pin the layout; not part of the conceptual API.
pub fn nchw_batch<'a>(
    images: impl Iterator<Item = &'a [u8]>,
    size: usize,
) -> Option<(usize, Vec<f32>)> {
    let plane = size * size;
    let mut flat: Vec<f32> = Vec::new();
    let mut count = 0usize;
    for rgb in images {
        if rgb.len() != plane * 3 {
            return None;
        }
        for c in 0..3 {
            flat.extend(
                rgb.chunks_exact(3)
                    .map(|px| f32::from(px[c]) * (2.0 / 255.0) - 1.0),
            );
        }
        count += 1;
    }
    Some((count, flat))
}

#[cfg(feature = "ai-search")]
mod siglip {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread::{self, JoinHandle};

    use crossbeam_channel::{Receiver, Select, Sender, bounded, unbounded};
    use dcs_domain::fingerprint::ContentFingerprint;
    use image::{DynamicImage, RgbaImage};
    use ort::ep::ExecutionProvider;
    use ort::session::Session;
    use ort::session::builder::GraphOptimizationLevel;
    use ort::value::Tensor;
    use rayon::prelude::*;
    use serde::Deserialize;
    use tokenizers::Tokenizer;

    use super::{EmbedRequest, EmbedResult, Embedder, EmbeddingError, nchw_batch, unit_vec};
    use crate::imaging::decode_thumbnail;

    /// ONNX Runtime SigLIP embedder. Owns one worker thread that holds the loaded
    /// sessions; they never cross a thread boundary, so no `Send`/lock juggling.
    pub struct SiglipEmbedder {
        text_tx: Sender<(u64, String)>,
        image_tx: Sender<EmbedRequest>,
        result_rx: Receiver<EmbedResult>,
        epoch: Arc<AtomicU64>,
        worker: JoinHandle<()>,
    }

    impl SiglipEmbedder {
        /// Load the embedded model and start the worker. Blocks until the model
        /// has loaded (or failed to), so the caller learns the outcome before
        /// reporting "ready" to the UI.
        pub fn new() -> Result<Self, EmbeddingError> {
            let (text_tx, text_rx) = unbounded::<(u64, String)>();
            let (image_tx, image_rx) = unbounded::<EmbedRequest>();
            let (result_tx, result_rx) = unbounded::<EmbedResult>();
            let (ready_tx, ready_rx) = bounded::<Result<(), EmbeddingError>>(1);
            let epoch = Arc::new(AtomicU64::new(0));
            let worker_epoch = Arc::clone(&epoch);

            let worker = thread::Builder::new()
                .name("dcs-embed".into())
                .spawn(move || worker_main(worker_epoch, text_rx, image_rx, result_tx, ready_tx))
                .map_err(|e| EmbeddingError::Load {
                    message: e.to_string(),
                })?;

            match ready_rx.recv() {
                Ok(Ok(())) => Ok(SiglipEmbedder {
                    text_tx,
                    image_tx,
                    result_rx,
                    epoch,
                    worker,
                }),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(EmbeddingError::WorkerGone),
            }
        }
    }

    impl Embedder for SiglipEmbedder {
        fn model_id(&self) -> &'static str {
            super::MODEL_ID
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

        fn set_epoch(&self, epoch: u64) {
            self.epoch.store(epoch, Ordering::Relaxed);
        }

        fn is_alive(&self) -> bool {
            !self.worker.is_finished()
        }
    }

    /// The model, baked into the binary. `build.rs` fetches, SHA-256-verifies,
    /// and stages these files (fp16 ONNX towers, tokenizer, config) and points
    /// the env vars at them; `include_bytes!` embeds them as `'static` slices —
    /// no runtime download, no disk read, works fully offline.
    mod embedded {
        pub static VISION: &[u8] = include_bytes!(env!("DCS_EMBED_VISION"));
        pub static TEXT: &[u8] = include_bytes!(env!("DCS_EMBED_TEXT"));
        pub static TOKENIZER: &[u8] = include_bytes!(env!("DCS_EMBED_TOKENIZER"));
        pub static CONFIG: &[u8] = include_bytes!(env!("DCS_EMBED_CONFIG"));
    }

    /// SigLIP's text padding token id. The tokenizer pads short queries up to the
    /// model's fixed sequence length with this id.
    const PAD_TOKEN_ID: u32 = 1;

    /// How many images to decode in parallel and embed in one batched forward.
    /// Decode (rayon) overlaps with inference, and a wide forward uses the GPU far
    /// better than many single-image calls — the main indexing speedup.
    const IMAGE_BATCH: usize = 8;

    /// Caption templates the query is wrapped in before encoding. SigLIP was
    /// trained on caption-like text, so `"a photo of a temple."` matches far
    /// better than the bare word; averaging several templates (the standard CLIP
    /// prompt ensemble) gives a sturdier, more general query vector across
    /// subjects.
    const PROMPT_TEMPLATES: [&str; 5] = [
        "a photo of a {}.",
        "a photo of {}.",
        "a close-up photo of a {}.",
        "a photograph of {}.",
        "{}.",
    ];

    /// The subset of the HF config the embedder needs. Read from the embedded
    /// `config.json` rather than hardcoded so a model-variant swap stays a
    /// build.rs change.
    #[derive(Deserialize)]
    struct ModelConfig {
        vision_config: VisionConfig,
        #[serde(default)]
        text_config: TextConfig,
    }

    #[derive(Deserialize)]
    struct VisionConfig {
        image_size: usize,
    }

    #[derive(Deserialize)]
    struct TextConfig {
        /// The ONNX export's config omits this field; every SigLIP v1 text tower
        /// uses 64 learned positions, so that is the serde default.
        #[serde(default = "default_max_len")]
        max_position_embeddings: usize,
    }

    impl Default for TextConfig {
        fn default() -> Self {
            TextConfig {
                max_position_embeddings: default_max_len(),
            }
        }
    }

    fn default_max_len() -> usize {
        64
    }

    /// The loaded sessions and the few constants the worker needs per inference.
    struct Loaded {
        vision: Session,
        text: Session,
        tokenizer: Tokenizer,
        image_size: usize,
        max_len: usize,
        /// Whether the vision session runs on the GPU. On a mid-session GPU
        /// failure the worker rebuilds on CPU once; this flag stops it retrying
        /// the GPU forever.
        gpu: bool,
    }

    /// Worker entry point: load the model (reporting success/failure on `ready`),
    /// then serve text queries first and the image sweep second until both queues
    /// disconnect. Returns (killing the worker — visible via
    /// [`Embedder::is_alive`]) on an unrecoverable inference failure, so the app
    /// shows an error instead of an eternally pending sweep.
    fn worker_main(
        epoch: Arc<AtomicU64>,
        text_rx: Receiver<(u64, String)>,
        image_rx: Receiver<EmbedRequest>,
        result_tx: Sender<EmbedResult>,
        ready_tx: Sender<Result<(), EmbeddingError>>,
    ) {
        let mut loaded = match load() {
            Ok(l) => l,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        let _ = ready_tx.send(Ok(()));

        loop {
            match next_job(&text_rx, &image_rx) {
                Some(Job::Text(job_epoch, query)) => {
                    if job_epoch != epoch.load(Ordering::Relaxed) {
                        continue; // stale query from a closed folder
                    }
                    let result = match embed_text(&mut loaded, &query) {
                        Some(vec) => EmbedResult::Text {
                            epoch: job_epoch,
                            query,
                            vec,
                        },
                        None => EmbedResult::TextFailed {
                            epoch: job_epoch,
                            query,
                        },
                    };
                    if result_tx.send(result).is_err() {
                        return;
                    }
                }
                Some(Job::Image(first)) => {
                    // Gather everything already queued (up to a batch) so decode
                    // runs in parallel and the model sees one wide forward instead
                    // of many. Requests from a closed folder are skipped before
                    // the expensive decode — this is the sweep's cancellation
                    // point (checked between work units, never mid-operation).
                    let current = epoch.load(Ordering::Relaxed);
                    let mut batch = Vec::with_capacity(IMAGE_BATCH);
                    if first.epoch == current {
                        batch.push(first);
                    }
                    while batch.len() < IMAGE_BATCH {
                        match image_rx.try_recv() {
                            Ok(req) if req.epoch == current => batch.push(req),
                            Ok(_) => {} // stale — drop without decoding
                            Err(_) => break,
                        }
                    }
                    if batch.is_empty() {
                        continue;
                    }
                    let results = match embed_image_batch(&mut loaded, &batch) {
                        Ok(results) => results,
                        // Inference broke. If the GPU session died (driver reset,
                        // device lost), rebuild on CPU and retry the batch once;
                        // a CPU failure is deterministic and would repeat forever,
                        // so die visibly instead.
                        Err(()) => {
                            if !loaded.gpu {
                                return;
                            }
                            let Ok(cpu) = session(embedded::VISION, false) else {
                                return;
                            };
                            loaded.vision = cpu;
                            loaded.gpu = false;
                            match embed_image_batch(&mut loaded, &batch) {
                                Ok(results) => results,
                                Err(()) => return,
                            }
                        }
                    };
                    for result in results {
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

    /// The next job, text queue first, blocking when both are empty. Exits
    /// (`None`) only when both queues have disconnected.
    fn next_job(
        text_rx: &Receiver<(u64, String)>,
        image_rx: &Receiver<EmbedRequest>,
    ) -> Option<Job> {
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

    /// Load both towers and the tokenizer from the embedded bytes. The vision
    /// tower (the heavy sweep) gets the GPU when one is usable; the text tower
    /// runs one short forward per typed query, so it stays on the CPU provider
    /// where its output is bit-stable.
    fn load() -> Result<Loaded, EmbeddingError> {
        let err = |message: String| EmbeddingError::Load { message };

        let config: ModelConfig =
            serde_json::from_slice(embedded::CONFIG).map_err(|e| err(e.to_string()))?;
        let tokenizer =
            Tokenizer::from_bytes(embedded::TOKENIZER).map_err(|e| err(e.to_string()))?;
        let image_size = config.vision_config.image_size;

        let (vision, gpu) = vision_session(image_size)?;
        let text = text_session().map_err(|e| err(e.to_string()))?;
        eprintln!(
            "dcs: embedding on {}",
            if gpu { "webgpu (gpu)" } else { "cpu" }
        );

        Ok(Loaded {
            vision,
            text,
            tokenizer,
            image_size,
            max_len: config.text_config.max_position_embeddings,
            gpu,
        })
    }

    /// The vision session on the best available backend: WebGPU probed with a
    /// throwaway forward (which also warms kernels), falling back to the CPU
    /// provider if the GPU is unavailable or broken — a missing-kernel failure
    /// surfaces here, where we can fall back, rather than silently dropping every
    /// embed batch later.
    fn vision_session(image_size: usize) -> Result<(Session, bool), EmbeddingError> {
        if let Ok(mut s) = session(embedded::VISION, true)
            && probe(&mut s, image_size).is_ok()
        {
            return Ok((s, true));
        }
        let s = session(embedded::VISION, false).map_err(|e| EmbeddingError::Load {
            message: e.to_string(),
        })?;
        Ok((s, false))
    }

    /// Build a session from embedded model bytes, optionally on the WebGPU
    /// execution provider. Optimization is capped at `Level3`: ONNX Runtime's
    /// `All` level crashes fusing these fp16 graphs' layer norms (upstream bug),
    /// and a failed full-graph optimize of a ~200 MB tower is a slow way to find
    /// that out at every startup. The EP is registered directly (not through
    /// `with_execution_providers`, whose platform filter skips WebGPU on macOS).
    fn session(bytes: &'static [u8], webgpu: bool) -> ort::Result<Session> {
        let mut builder =
            Session::builder()?.with_optimization_level(GraphOptimizationLevel::Level3)?;
        if webgpu {
            ort::ep::WebGPU::default().register(&mut builder)?;
        }
        builder.commit_from_memory(bytes)
    }

    /// The text session: CPU, with a small thread pool and no spin-waiting. It
    /// runs one tiny forward per typed query; ORT's default (a spinning pool the
    /// width of the machine) would idle-burn cores against the rayon decode pool
    /// and the UI thread.
    fn text_session() -> ort::Result<Session> {
        Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(2)?
            .with_intra_op_spinning(false)?
            .commit_from_memory(embedded::TEXT)
    }

    /// One throwaway vision forward to validate the session end-to-end. Runs at
    /// the full batch shape — GPU pipelines are compiled per shape, so this is
    /// also the warmup that keeps the first real batch from hitching.
    fn probe(session: &mut Session, image_size: usize) -> ort::Result<()> {
        let flat = vec![0.0f32; IMAGE_BATCH * 3 * image_size * image_size];
        let input = Tensor::from_array(([IMAGE_BATCH, 3, image_size, image_size], flat))?;
        session.run(ort::inputs!["pixel_values" => input])?;
        Ok(())
    }

    /// Embed a batch of images in one forward. Decode + resize runs in parallel
    /// on the CPU (rayon, no session calls off the worker thread); the pixels are
    /// then uploaded as a single tensor and run through the vision tower once.
    /// Per-image vectors are identical to embedding them singly. A decode failure
    /// drops just that image (`Ok` with fewer results); an inference failure is
    /// `Err` so the caller can rebuild the session and retry.
    fn embed_image_batch(
        loaded: &mut Loaded,
        batch: &[EmbedRequest],
    ) -> Result<Vec<EmbedResult>, ()> {
        let size = loaded.image_size;
        let prepared: Vec<(u64, ContentFingerprint, Vec<u8>)> = batch
            .par_iter()
            .filter_map(|req| Some((req.epoch, req.fingerprint, decode_rgb(req, size)?)))
            .collect();
        if prepared.is_empty() {
            return Ok(Vec::new());
        }
        let (count, mut flat) =
            nchw_batch(prepared.iter().map(|(_, _, rgb)| rgb.as_slice()), size).ok_or(())?;
        // Pad a partial batch up to the fixed shape: GPU pipelines and ORT's
        // memory patterns are per input shape, so one constant shape avoids a
        // recompile/re-plan on every partial batch. The padded rows are discarded
        // below.
        let padded = count.max(IMAGE_BATCH);
        flat.resize(padded * 3 * size * size, 0.0);
        let pixels = Tensor::from_array(([padded, 3, size, size], flat)).map_err(|_| ())?;
        let outputs = loaded
            .vision
            .run(ort::inputs!["pixel_values" => pixels])
            .map_err(|_| ())?;
        let (shape, features) = outputs
            .get("pooler_output")
            .ok_or(())?
            .try_extract_tensor::<f32>()
            .map_err(|_| ())?;
        // Take the row width from the reported shape, not by dividing the flat
        // length — a padded or reshaped output must fail loudly, not cross-wire
        // vectors onto the wrong photos.
        if shape.len() != 2 || shape[0] as usize != padded {
            return Err(());
        }
        let dim = shape[1] as usize;
        Ok(prepared
            .iter()
            .zip(features.chunks_exact(dim))
            .filter_map(|((epoch, fingerprint, _), row)| {
                // A non-finite row (GPU fp16 gone wrong) must not be cached — it
                // would poison ranking for that photo until a model-ID bump.
                if !row.iter().all(|x| x.is_finite()) {
                    return None;
                }
                Some(EmbedResult::Image {
                    epoch: *epoch,
                    fingerprint: *fingerprint,
                    vec: unit_vec(row),
                })
            })
            .collect())
    }

    /// Embed a text query to a unit vector comparable to image vectors. The query
    /// is expanded through [`PROMPT_TEMPLATES`] and the per-template vectors are
    /// averaged (then renormalized) — a sturdier query than encoding the bare
    /// word once. `None` on any failure; the caller reports it as
    /// [`EmbedResult::TextFailed`].
    fn embed_text(loaded: &mut Loaded, query: &str) -> Option<Vec<f32>> {
        let rows: Vec<Vec<i64>> = PROMPT_TEMPLATES
            .iter()
            .map(|t| token_ids(loaded, &t.replace("{}", query)))
            .collect::<Option<_>>()?;
        let n = rows.len();
        let flat: Vec<i64> = rows.into_iter().flatten().collect();
        // One batched forward over every template: (n_templates, seq) → (n_templates, dim).
        let input = Tensor::from_array(([n, loaded.max_len], flat)).ok()?;
        let outputs = loaded.text.run(ort::inputs!["input_ids" => input]).ok()?;
        let (shape, features) = outputs
            .get("pooler_output")?
            .try_extract_tensor::<f32>()
            .ok()?;
        if shape.len() != 2 || shape[0] as usize != n {
            return None;
        }
        let dim = shape[1] as usize;
        let mut mean = vec![0.0f32; dim];
        for row in features.chunks_exact(dim) {
            for (m, v) in mean.iter_mut().zip(row) {
                *m += v / n as f32;
            }
        }
        if !mean.iter().all(|x| x.is_finite()) {
            return None;
        }
        Some(unit_vec(&mean))
    }

    /// Decode one image and squash-resize it to the model square, returning raw
    /// RGB bytes (`size*size*3`). Decoded at twice the model edge so both axes of
    /// any aspect ratio up to 2:1 arrive at or above `size` — a contain-fit at
    /// `size` would leave the short axis below it and the final resize would
    /// upscale, losing exactly the detail this fresh decode is for. SigLIP's
    /// processor resizes **anisotropically** to `size×size` (no center crop), so
    /// `resize_exact` matches the training distribution rather than
    /// `resize_to_fill` (which would crop the edges). Pure CPU and free of
    /// session calls, so it's safe across rayon threads. `None` on decode
    /// failure.
    fn decode_rgb(req: &EmbedRequest, size: usize) -> Option<Vec<u8>> {
        let thumb = decode_thumbnail(&req.path, req.orientation, (size * 2) as u32, None)?;
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

    /// Tokenize a query and pad/truncate to the model's fixed sequence length,
    /// as `i64` — the dtype the ONNX text tower expects.
    fn token_ids(loaded: &Loaded, query: &str) -> Option<Vec<i64>> {
        let encoding = loaded.tokenizer.encode(query, true).ok()?;
        let mut ids = encoding.get_ids().to_vec();
        ids.truncate(loaded.max_len);
        ids.resize(loaded.max_len, PAD_TOKEN_ID);
        Some(ids.into_iter().map(i64::from).collect())
    }
}

#[cfg(not(feature = "ai-search"))]
mod stub {
    use super::{EmbedRequest, EmbedResult, Embedder, EmbeddingError};

    /// Placeholder for builds without the `ai-search` feature: construction
    /// always fails with a clear message, which the UI surfaces through its
    /// normal AI-error state. The trait methods are unreachable (no instance can
    /// exist) but are implemented as no-ops.
    pub struct SiglipEmbedder;

    impl SiglipEmbedder {
        /// Always fails: this build does not include the model or the runtime.
        pub fn new() -> Result<Self, EmbeddingError> {
            Err(EmbeddingError::Load {
                message: "AI search is not included in this build".into(),
            })
        }
    }

    impl Embedder for SiglipEmbedder {
        fn model_id(&self) -> &'static str {
            super::MODEL_ID
        }

        fn embed_image(&self, _req: EmbedRequest) {}

        fn embed_text(&self, _epoch: u64, _query: String) {}

        fn poll(&self) -> Vec<EmbedResult> {
            Vec::new()
        }
    }
}
