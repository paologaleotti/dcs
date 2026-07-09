//! Build-time model embedding. With the `ai-search` feature on, the SigLIP ONNX
//! model is baked into the binary.
//!
//! Sources the four model files (from `DCS_MODEL_DIR` if set, else a pinned
//! HuggingFace download), verifies their SHA-256, and stages them for
//! `include_bytes!`. Nothing is ever committed to git — only the pin (repo +
//! revision in `model_revision.txt`, hashes below).

use std::path::{Path, PathBuf};

/// The pinned commit, kept in a plain text file (`model_revision.txt`) so this
/// build script and CI read the exact same source. `trim()` at every use — the
/// file has a trailing newline.
const MODEL_REVISION: &str = include_str!("model_revision.txt");

/// Repo the embedded model comes from: the official ONNX export of
/// `google/siglip-base-patch16-384`, split into per-tower fp16 graphs. The
/// revision is `MODEL_REVISION`.
const REPO: &str = "Xenova/siglip-base-patch16-384";

/// The files to source. `remote` is the path inside the repo; `name` is the flat
/// local name used in the download cache and `DCS_MODEL_DIR`. `sha256` is the
/// as-downloaded hash; leave empty to self-pin on first build (the script prints
/// the computed hash and a warning to paste it back here, locking future builds
/// against drift/tampering).
struct ModelFile {
    name: &'static str,
    remote: &'static str,
    sha256: &'static str,
}
const FILES: [ModelFile; 4] = [
    ModelFile {
        name: "config.json",
        remote: "config.json",
        sha256: "25dc1426d9874b8ca99420378d32df58461777b0a9868d45a888775c02d25090",
    },
    ModelFile {
        name: "tokenizer.json",
        remote: "tokenizer.json",
        sha256: "798a8118466a62b99c98fc111134d76b0905f92debc0536d2602aa5bd97c0ab9",
    },
    ModelFile {
        name: "vision_model_fp16.onnx",
        remote: "onnx/vision_model_fp16.onnx",
        sha256: "bcc53aac76b42b2d0e8c86a89fb89005ae52dcd0dcabf887a60b890a64a3a971",
    },
    ModelFile {
        name: "text_model_fp16.onnx",
        remote: "onnx/text_model_fp16.onnx",
        sha256: "301ef4194c2995fcdc41789c2386f7fdfcae53acb73b70ffd6feefd69a2241e9",
    },
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=model_revision.txt");
    println!("cargo:rerun-if-env-changed=DCS_MODEL_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");

    // Without AI search there is nothing to download or embed; the embedding
    // module compiles to a stub that never touches these files.
    if std::env::var_os("CARGO_FEATURE_AI_SEARCH").is_none() {
        return;
    }

    let (src, refetchable) = source_dir();

    for f in &FILES {
        let path = src.join(f.name);
        verify(&path, f, refetchable);
        // Re-verify whenever a staged file changes: rustc's dep-info tracks the
        // include_bytes! sources, so without this a swapped file would be
        // embedded on the next compile with the SHA-256 pin never re-checked.
        println!("cargo:rerun-if-changed={}", path.display());
    }

    emit("DCS_EMBED_VISION", &src.join("vision_model_fp16.onnx"));
    emit("DCS_EMBED_TEXT", &src.join("text_model_fp16.onnx"));
    emit("DCS_EMBED_TOKENIZER", &src.join("tokenizer.json"));
    emit("DCS_EMBED_CONFIG", &src.join("config.json"));
}

fn emit(key: &str, path: &Path) {
    println!("cargo:rustc-env={key}={}", path.display());
}

/// Where the source files live: `DCS_MODEL_DIR` if set (offline / CI / your own
/// copy — files under their flat `name`s), otherwise a pinned download into a
/// **stable, revision-keyed cache** under the target dir — shared across every
/// build unit and build mode, so the ~410 MB download happens at most once per
/// revision per machine (not once per feature/profile combination). The second
/// return says whether a hash-mismatching file may be re-downloaded (the cache)
/// or is the user's problem (`DCS_MODEL_DIR`).
fn source_dir() -> (PathBuf, bool) {
    if let Some(dir) = std::env::var_os("DCS_MODEL_DIR") {
        return (PathBuf::from(dir), false);
    }
    let dl = download_cache();
    std::fs::create_dir_all(&dl).expect("create download cache dir");
    for f in &FILES {
        let dest = dl.join(f.name);
        if dest.exists() {
            continue; // cached from a prior build (any unit/mode)
        }
        download(f.remote, &dest);
    }
    (dl, true)
}

/// A stable cache directory keyed by revision, under the workspace target dir.
/// Survives feature/profile switches; only `cargo clean` (or deleting it) clears
/// it. Honors `CARGO_TARGET_DIR`.
fn download_cache() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("target")
        });
    target.join("dcs-model-cache").join(MODEL_REVISION.trim())
}

fn download(remote: &str, dest: &Path) {
    let url = format!(
        "https://huggingface.co/{REPO}/resolve/{}/{remote}",
        MODEL_REVISION.trim()
    );
    println!("cargo:warning=downloading {url}");
    // A read timeout aborts a stalled connection instead of hanging the build
    // forever; there is deliberately no overall timeout — the files are large
    // and links vary.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(30))
        .timeout_read(std::time::Duration::from_secs(120))
        .build();
    let resp = agent
        .get(&url)
        .call()
        .unwrap_or_else(|e| panic!("download {remote}: {e}"));
    let mut reader = resp.into_reader();
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).expect("create temp file");
    std::io::copy(&mut reader, &mut file).expect("write download");
    // Fsync before rename so a crash can't leave a torn file that the next build's
    // `exists()` check would treat as a complete cached download.
    file.sync_all().expect("fsync download");
    drop(file);
    std::fs::rename(&tmp, dest).expect("rename download");
}

/// Verify a file's SHA-256 against its pin. Empty pin → self-pin: print the hash
/// and warn the developer to paste it into `FILES` to lock it. In the download
/// cache (`refetchable`) a mismatching file — a torn or tampered cache entry —
/// is deleted and re-downloaded once before giving up; in `DCS_MODEL_DIR` it is
/// the user's file and only fails.
fn verify(path: &Path, f: &ModelFile, refetchable: bool) {
    if let Some(got) = check(path, f) {
        if !refetchable {
            mismatch_panic(f, &got);
        }
        println!(
            "cargo:warning={} sha256 mismatch in cache; re-downloading",
            f.name
        );
        std::fs::remove_file(path).unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
        download(f.remote, path);
        if let Some(got) = check(path, f) {
            mismatch_panic(f, &got);
        }
    }
}

/// `Some(actual_hash)` when the file's SHA-256 does not match the pin (self-pin
/// warnings count as matching).
fn check(path: &Path, f: &ModelFile) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let got = hex(&Sha256::digest(&bytes));
    if f.sha256.is_empty() {
        println!(
            "cargo:warning=PIN {}: sha256 = {got} (paste into build.rs FILES to lock)",
            f.name
        );
        return None;
    }
    (got != f.sha256).then_some(got)
}

fn mismatch_panic(f: &ModelFile, got: &str) -> ! {
    panic!(
        "{} sha256 mismatch:\n  expected {}\n  got      {got}\nrefusing to embed an unpinned file",
        f.name, f.sha256
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
