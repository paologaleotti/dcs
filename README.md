# dcs — Digital Contact Sheet

A fast, minimal, **keyboard-first digital contact sheet** — closer to an analog
contact sheet than to a photo editor. Scan, cull, tag, and export thousands of
JPEGs without heavy editing software. Originals are never modified.

> **Status:** pre-alpha.

## TODO

- no AI build/cuda build/metal build (macos only)
- export review and refactor
- uxui improvements

## Features

- Keyboard-first grid + gallery over a whole folder, 60 fps scrolling.
- Non-destructive culling (accept/reject) and tagging; verdicts and tags persist.
- Derived grouping, bursts, sort — recomputed, never persisted.
- **AI semantic search** — type "temple" and get temple photos. Local, offline,
  no API. See [AI search](#ai-search) below.
- Pure export planner; copy-only, never overwrites.

## Prerequisites

- **Rust** stable (`rustup` recommended).
- **NASM** + **CMake** — `turbojpeg` builds libjpeg-turbo's SIMD from source.
  (CMake ships on most systems; install NASM via your package manager.)
- **Linux only** — GUI dev headers:
  ```sh
  sudo apt-get install -y libgtk-3-dev libxkbcommon-dev libwayland-dev \
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
    libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev pkg-config
  ```

## Build & run

```sh
cargo build --workspace        # build everything
cargo run -p dcs-ui            # launch the app (binary name: dcs)
```

Release build:

```sh
cargo build --release -p dcs-ui --bin dcs
```

> **First build fetches the AI model (~800 MB, once).** `build.rs` downloads the
> pinned SigLIP model, verifies its SHA-256, fp16-converts it, and embeds it in the
> binary — see [AI search](#ai-search). It's cached per revision under `target/`, so
> only the first build (or a `cargo clean`) pays the download.

## AI search

Type "temple", get temple photos — a local
[SigLIP](https://huggingface.co/google/siglip-base-patch16-384) image–text model
(candle), fully offline. **The model is always embedded in the binary** (no runtime
download, works out of the box), which adds **~390 MB** to the executable.

`build.rs` handles it automatically: it fetches the pinned weights, verifies their
SHA-256, converts fp32 → fp16 (halving the size), and bakes them in via
`include_bytes!`.

**Offline / air-gapped builds:** pre-place the three files (`config.json`,
`tokenizer.json`, `model.safetensors` from the pinned revision) in a directory and
point `build.rs` at it — no download:

```sh
DCS_MODEL_DIR=/path/to/model cargo build --release -p dcs-ui --bin dcs
```

**Updating the model:** edit the pinned commit in
**`crates/dcs-io/model_revision.txt`** (the single source, read by `build.rs` and
CI). The next build prints the new SHA-256 hashes; paste them into
`crates/dcs-io/build.rs` to lock them against drift.

### GPU acceleration

Embedding inference picks the best backend automatically, with CPU fallback:

| Platform | Backend | How |
|---|---|---|
| macOS | **Metal** | automatic (enabled by default on macOS) |
| Linux / Windows + Nvidia | **CUDA** | `--features cuda` (needs the CUDA toolkit) |
| anything else | **CPU** | automatic fallback |

```sh
cargo build --release -p dcs-ui --bin dcs --features cuda    # Nvidia
```

## Test & lint

```sh
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

## Workspace layout

Four crates, dependencies point downward only:

| Crate | Role |
|---|---|
| `dcs-ui` | egui binary: grid/gallery views, ephemeral UI state |
| `dcs-app` | conductor: session, command registry, dispatch, undo |
| `dcs-io` | infrastructure behind traits: imaging, scan, persistence, embeddings |
| `dcs-domain` | pure core: types + pure functions (no I/O, no async, no egui) |

The authoritative design lives in [`spec.md`](spec.md).

## Licensing

- **dcs** is licensed under **MIT OR Apache-2.0** (at your option).
- The embedded **SigLIP** model and tokenizer (`google/siglip-base-patch16-384`)
  are © Google, licensed under **Apache-2.0**. Since every build ships the model,
  distributions must include the model attribution and the Apache-2.0 license text
  — see [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). The weights are
  converted to fp16 for embedding; no other modification is made.
