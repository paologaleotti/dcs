//! Build-time model embedding. The SigLIP model is always baked into the binary.
//!
//! Sources the three model files (from `DCS_MODEL_DIR` if set, else a pinned
//! HuggingFace download), verifies their SHA-256, converts the weights from
//! fp32 → fp16 (halving the embedded size with no GPU-runtime quality change), and
//! writes them into `OUT_DIR` for `include_bytes!`. Nothing is ever committed to
//! git — only the pin (repo + revision in `model_revision.txt`, hashes below).

use std::path::{Path, PathBuf};

/// The pinned commit, kept in a plain text file (`model_revision.txt`) so this
/// build script and CI read the exact same source. `trim()` at every use — the
/// file has a trailing newline.
const MODEL_REVISION: &str = include_str!("model_revision.txt");

/// Repo the embedded weights come from. The revision is `MODEL_REVISION`.
const REPO: &str = "google/siglip-base-patch16-384";

/// The files to source. `sha256` is the *fp32* (as-downloaded) hash; leave empty
/// to self-pin on first build (the script prints the computed hash and a warning
/// to paste it back here, locking future builds against drift/tampering).
struct ModelFile {
    name: &'static str,
    sha256: &'static str,
}
const FILES: [ModelFile; 3] = [
    ModelFile {
        name: "config.json",
        sha256: "bd23a8a92607ff1ebdd84b625772246d9b0160d0a7f4b63bde2bd3ae1baa21de",
    },
    ModelFile {
        name: "tokenizer.json",
        sha256: "c6e405cb7c670d56636a9402c81023a55bc6c3c53d89cf02b92f5c5005bfe920",
    },
    ModelFile {
        name: "model.safetensors",
        sha256: "f273e98edc393ec5ffea71518b0c9ab4b0e8dd2be43affb22d49ff659fb28605",
    },
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=model_revision.txt");
    println!("cargo:rerun-if-env-changed=DCS_MODEL_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let src = source_dir(&out);

    for f in &FILES {
        verify(&src.join(f.name), f);
    }

    // fp32 → fp16 conversion (cached: skip if already produced).
    let weights_fp16 = out.join("model.fp16.safetensors");
    if !weights_fp16.exists() {
        convert_fp16(&src.join("model.safetensors"), &weights_fp16);
    }

    emit("DCS_EMBED_WEIGHTS", &weights_fp16);
    emit("DCS_EMBED_TOKENIZER", &src.join("tokenizer.json"));
    emit("DCS_EMBED_CONFIG", &src.join("config.json"));
}

fn emit(key: &str, path: &Path) {
    println!("cargo:rustc-env={key}={}", path.display());
}

/// Where the fp32 source files live: `DCS_MODEL_DIR` if set (offline / CI / your
/// own copy), otherwise a pinned download into a **stable, revision-keyed cache**
/// under the target dir — shared across every build unit and build mode, so the
/// ~800 MB download happens at most once per revision per machine (not once per
/// feature/profile combination).
fn source_dir(_out: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("DCS_MODEL_DIR") {
        return PathBuf::from(dir);
    }
    let dl = download_cache();
    std::fs::create_dir_all(&dl).expect("create download cache dir");
    for f in &FILES {
        let dest = dl.join(f.name);
        if dest.exists() {
            continue; // cached from a prior build (any unit/mode)
        }
        download(f.name, &dest);
    }
    dl
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

fn download(name: &str, dest: &Path) {
    let url = format!(
        "https://huggingface.co/{REPO}/resolve/{}/{name}",
        MODEL_REVISION.trim()
    );
    println!("cargo:warning=downloading {url}");
    let resp = ureq::get(&url)
        .call()
        .unwrap_or_else(|e| panic!("download {name}: {e}"));
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
/// and warn the developer to paste it into `FILES` to lock it.
fn verify(path: &Path, f: &ModelFile) {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let got = hex(&Sha256::digest(&bytes));
    if f.sha256.is_empty() {
        println!(
            "cargo:warning=PIN {}: sha256 = {got} (paste into build.rs FILES to lock)",
            f.name
        );
    } else if got != f.sha256 {
        panic!(
            "{} sha256 mismatch:\n  expected {}\n  got      {got}\nrefusing to embed an unpinned file",
            f.name, f.sha256
        );
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read an fp32 safetensors file and write an fp16 copy. Only `F32` tensors are
/// down-converted; every other dtype (this model has none, but be exact) is copied
/// verbatim. Written atomically (`.part` → rename) so an interrupted convert never
/// leaves a half-written file the `exists()` cache check would trust.
fn convert_fp16(src: &Path, dest: &Path) {
    use half::f16;
    use safetensors::tensor::{Dtype, SafeTensors, TensorView};

    let raw = std::fs::read(src).expect("read weights");
    let st = SafeTensors::deserialize(&raw).expect("parse safetensors");

    // Owned converted buffers; TensorViews below borrow these.
    let mut owned: Vec<(String, Dtype, Vec<usize>, Vec<u8>)> = Vec::new();
    for (name, view) in st.tensors() {
        let shape = view.shape().to_vec();
        match view.dtype() {
            Dtype::F32 => {
                let data = view.data();
                let mut out = Vec::with_capacity(data.len() / 2);
                for chunk in data.chunks_exact(4) {
                    let v = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    out.extend_from_slice(&f16::from_f32(v).to_le_bytes());
                }
                owned.push((name, Dtype::F16, shape, out));
            }
            other => owned.push((name, other, shape, view.data().to_vec())),
        }
    }

    let views: Vec<(&str, TensorView)> = owned
        .iter()
        .map(|(name, dtype, shape, data)| {
            (
                name.as_str(),
                TensorView::new(*dtype, shape.clone(), data).expect("build tensor view"),
            )
        })
        .collect();
    let tmp = dest.with_extension("part");
    safetensors::tensor::serialize_to_file(views, None, &tmp).expect("write fp16 safetensors");
    // Fsync before rename, like the download path — otherwise a crash can leave a
    // torn fp16 file that the next build's `exists()` check would trust.
    std::fs::File::open(&tmp)
        .and_then(|f| f.sync_all())
        .expect("fsync fp16 safetensors");
    std::fs::rename(&tmp, dest).expect("rename fp16 safetensors");
}
