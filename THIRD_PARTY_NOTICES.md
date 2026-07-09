# Third-party notices

dcs distributes the following third-party material. Every dcs build embeds the
model, so this file must accompany every distribution.

## SigLIP model — `google/siglip-base-patch16-384`

- **Source:** https://huggingface.co/google/siglip-base-patch16-384 (weights),
  embedded as the fp16 ONNX export published at
  https://huggingface.co/Xenova/siglip-base-patch16-384
- **Copyright:** © Google LLC.
- **License:** Apache License, Version 2.0.
- **Used for:** local image–text embeddings powering AI search (the model
  weights and tokenizer are embedded in the dcs binary).
- **Modification:** the weights are converted to the ONNX format at fp16
  precision (the export above); no other change is made.

The model is used under the terms of the Apache License, Version 2.0. A copy of
the license is reproduced below.

> The SigLIP weights originate from Google's `big_vision` project
> (https://github.com/google-research/big_vision), released under Apache-2.0.

## ONNX Runtime

- **Source:** https://github.com/microsoft/onnxruntime (statically linked;
  prebuilt by the `ort` project, https://ort.pyke.io)
- **Copyright:** © Microsoft Corporation.
- **License:** MIT License.
- **Used for:** running the embedded model (AI search inference).

## Dawn (WebGPU implementation) — `libwebgpu_dawn`

- **Source:** https://dawn.googlesource.com/dawn (shipped as a shared library
  next to the dcs binary, as part of ONNX Runtime's WebGPU execution provider)
- **Copyright:** © the Dawn & Tint Authors.
- **License:** BSD 3-Clause License.
- **Used for:** GPU acceleration of AI search inference (Metal / DirectX 12 /
  Vulkan).

---

## Apache License 2.0

The full text of the Apache License, Version 2.0 applies to the SigLIP model
above and is available at:

    https://www.apache.org/licenses/LICENSE-2.0

A verbatim copy must be included with any distribution that bundles the model.
(Place the full `LICENSE-2.0.txt` alongside this file when cutting a release; it
is omitted from the source tree to avoid duplicating ~11 KB of boilerplate, but
the release artifacts must carry it.)
