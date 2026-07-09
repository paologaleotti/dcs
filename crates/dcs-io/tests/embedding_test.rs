//! End-to-end tests for the ONNX SigLIP embedder: real model, real inference
//! (CPU or GPU, whichever the worker picks). Model loading is slow, so all
//! tests share one embedder via `OnceLock`.
#![cfg(feature = "ai-search")]

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::Orientation;
use dcs_io::embedding::{
    EmbedRequest, EmbedResult, Embedder, SiglipEmbedder, nchw_batch, unit_vec,
};
use image::{Rgb, RgbImage};

/// The shared embedder plus a guard serializing tests around it: `poll` drains
/// one shared channel, so concurrent tests would steal each other's results.
/// Any results a previous (possibly failed) test left behind are drained so
/// one failure can't cascade into misleading ones.
fn embedder() -> (MutexGuard<'static, ()>, &'static SiglipEmbedder) {
    static EMBEDDER: OnceLock<SiglipEmbedder> = OnceLock::new();
    static GUARD: Mutex<()> = Mutex::new(());
    let guard = GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let embedder = EMBEDDER.get_or_init(|| SiglipEmbedder::new().expect("load embedded model"));
    while !embedder.poll().is_empty() {
        std::thread::sleep(Duration::from_millis(10));
    }
    (guard, embedder)
}

/// A temp JPEG whose name can't collide with a concurrent test run of another
/// checkout (per-process suffix).
fn write_jpeg(name: &str) -> std::path::PathBuf {
    let mut img = RgbImage::new(640, 480);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
    }
    let path = std::env::temp_dir().join(format!("{}_{name}", std::process::id()));
    img.save(&path).expect("encode jpeg");
    path
}

fn fingerprint(seed: u8) -> ContentFingerprint {
    ContentFingerprint::from_bytes([seed; 32])
}

/// Poll the embedder until `want` results arrive (results may straddle polls).
fn poll_results(e: &SiglipEmbedder, want: usize) -> Vec<EmbedResult> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut out = Vec::new();
    while out.len() < want {
        out.extend(e.poll());
        assert!(Instant::now() < deadline, "timed out waiting for results");
        std::thread::sleep(Duration::from_millis(20));
    }
    out
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn image_vec(result: &EmbedResult) -> (&ContentFingerprint, &[f32]) {
    match result {
        EmbedResult::Image {
            fingerprint, vec, ..
        } => (fingerprint, vec),
        other => panic!("expected image result, got {other:?}"),
    }
}

fn text_vec(result: &EmbedResult) -> &[f32] {
    match result {
        EmbedResult::Text { vec, .. } => vec,
        other => panic!("expected text result, got {other:?}"),
    }
}

#[test]
fn image_embed_is_a_768_dim_unit_vector_and_batches_reproduce_it() {
    let (_guard, e) = embedder();
    let path = write_jpeg("dcs_embed_single.jpg");
    e.set_epoch(7);

    let request = |seed: u8| EmbedRequest {
        epoch: 7,
        fingerprint: fingerprint(seed),
        path: path.clone(),
        orientation: Orientation::Normal,
    };

    // One image alone.
    e.embed_image(request(1));
    let alone = poll_results(e, 1);
    let (_, reference) = image_vec(&alone[0]);
    assert!(
        matches!(alone[0], EmbedResult::Image { epoch: 7, .. }),
        "epoch must round-trip on the result"
    );
    assert_eq!(reference.len(), 768);
    let norm: f32 = reference.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "unit norm, got {norm}");

    // The same image queued three times lands (in whatever batching the worker
    // chose) on the same vector. GPU fp16 batching may reorder accumulation, so
    // the bound is ranking-safe rather than bit-exact.
    e.embed_image(request(2));
    e.embed_image(request(3));
    e.embed_image(request(4));
    let batched = poll_results(e, 3);
    for result in &batched {
        let (_, vec) = image_vec(result);
        let sim = cosine(reference, vec);
        assert!(sim > 0.999, "batched output diverged: cosine {sim}");
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn decode_failure_drops_only_that_image() {
    let (_guard, e) = embedder();
    let path = write_jpeg("dcs_embed_mixed.jpg");
    e.set_epoch(9);

    e.embed_image(EmbedRequest {
        epoch: 9,
        fingerprint: fingerprint(10),
        path: std::path::PathBuf::from("/nonexistent/bogus.jpg"),
        orientation: Orientation::Normal,
    });
    e.embed_image(EmbedRequest {
        epoch: 9,
        fingerprint: fingerprint(11),
        path: path.clone(),
        orientation: Orientation::Normal,
    });

    // Exactly one result: the good image. The bogus one degrades silently.
    let results = poll_results(e, 1);
    let (fp, vec) = image_vec(&results[0]);
    assert_eq!(fp, &fingerprint(11));
    assert_eq!(vec.len(), 768);
    // Settle briefly: the bogus image must not produce a late second result.
    std::thread::sleep(Duration::from_millis(300));
    assert!(e.poll().is_empty(), "bogus image must yield no result");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn stale_epoch_requests_are_skipped() {
    let (_guard, e) = embedder();
    let path = write_jpeg("dcs_embed_stale.jpg");
    e.set_epoch(20);

    // Queued for an epoch that is no longer current — the worker must drop it
    // without decoding.
    e.embed_image(EmbedRequest {
        epoch: 19,
        fingerprint: fingerprint(20),
        path: path.clone(),
        orientation: Orientation::Normal,
    });
    // A current-epoch request right behind it still resolves.
    e.embed_image(EmbedRequest {
        epoch: 20,
        fingerprint: fingerprint(21),
        path: path.clone(),
        orientation: Orientation::Normal,
    });
    let results = poll_results(e, 1);
    let (fp, _) = image_vec(&results[0]);
    assert_eq!(fp, &fingerprint(21));
    std::thread::sleep(Duration::from_millis(300));
    assert!(e.poll().is_empty(), "stale request must yield no result");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn text_embeds_rank_related_terms_closer() {
    let (_guard, e) = embedder();
    e.set_epoch(1);
    e.embed_text(1, "dog".into());
    e.embed_text(1, "puppy".into());
    e.embed_text(1, "spreadsheet".into());
    let results = poll_results(e, 3);

    let vec_for = |query: &str| -> &[f32] {
        results
            .iter()
            .find(|r| matches!(r, EmbedResult::Text { query: q, .. } if q == query))
            .map(text_vec)
            .unwrap_or_else(|| panic!("no result for {query}"))
    };

    let dog = vec_for("dog");
    assert_eq!(dog.len(), 768);
    let norm: f32 = dog.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "unit norm, got {norm}");

    let related = cosine(dog, vec_for("puppy"));
    let unrelated = cosine(dog, vec_for("spreadsheet"));
    assert!(
        related > unrelated,
        "dog↔puppy ({related}) should beat dog↔spreadsheet ({unrelated})"
    );
}

#[test]
fn text_result_echoes_epoch_and_query() {
    let (_guard, e) = embedder();
    e.set_epoch(42);
    e.embed_text(42, "temple".into());
    let results = poll_results(e, 1);
    let found = results
        .iter()
        .any(|r| matches!(r, EmbedResult::Text { epoch: 42, query, .. } if query == "temple"));
    assert!(found, "epoch/query must round-trip on the result");
}

#[test]
fn unit_vec_normalizes_and_leaves_zero_alone() {
    let v = unit_vec(&[3.0, 4.0]);
    assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);

    let zero = unit_vec(&[0.0, 0.0, 0.0]);
    assert_eq!(zero, vec![0.0, 0.0, 0.0]);
}

#[test]
fn nchw_batch_lays_out_channel_planes_scaled_to_unit_range() {
    // One 2×2 image: four pixels with distinct channel values.
    #[rustfmt::skip]
    let rgb: Vec<u8> = vec![
        0, 128, 255,   10, 20, 30,
        40, 50, 60,    255, 0, 128,
    ];
    let (count, flat) = nchw_batch([rgb.as_slice()].into_iter(), 2).expect("well-sized input");
    assert_eq!(count, 1);
    assert_eq!(flat.len(), 3 * 2 * 2);

    let scale = |b: u8| f32::from(b) * (2.0 / 255.0) - 1.0;
    // R plane, then G, then B — each in pixel (row-major) order.
    let expected: Vec<f32> = [[0u8, 10, 40, 255], [128, 20, 50, 0], [255, 30, 60, 128]]
        .iter()
        .flatten()
        .map(|&b| scale(b))
        .collect();
    assert_eq!(flat, expected);

    assert!((scale(0) - -1.0).abs() < 1e-6);
    assert!((scale(255) - 1.0).abs() < 1e-6);

    // A mis-sized image must refuse the whole batch — silently shifting every
    // later image onto the wrong vector is the failure mode this guards.
    assert!(nchw_batch([&rgb[..9]].into_iter(), 2).is_none());
}

#[test]
fn unit_vec_leaves_non_finite_input_unnormalized() {
    let v = unit_vec(&[f32::NAN, 1.0]);
    assert!(v[0].is_nan(), "non-finite norm must not divide");
}
