# Third-party notices

dcs distributes the following third-party material. Every dcs build embeds the
model, so this file must accompany every distribution.

## SigLIP model — `google/siglip-base-patch16-384`

- **Source:** https://huggingface.co/google/siglip-base-patch16-384
- **Copyright:** © Google LLC.
- **License:** Apache License, Version 2.0.
- **Used for:** local image–text embeddings powering AI search (the model weights
  and tokenizer are embedded in the dcs binary).
- **Modification:** the weights are converted from fp32 to fp16 for embedding; no
  other change is made.

The model is used under the terms of the Apache License, Version 2.0. A copy of
the license is reproduced below.

> The SigLIP weights originate from Google's `big_vision` project
> (https://github.com/google-research/big_vision), released under Apache-2.0.

---

## Apache License 2.0

The full text of the Apache License, Version 2.0 applies to the SigLIP model
above and is available at:

    https://www.apache.org/licenses/LICENSE-2.0

A verbatim copy must be included with any distribution that bundles the model.
(Place the full `LICENSE-2.0.txt` alongside this file when cutting a release; it
is omitted from the source tree to avoid duplicating ~11 KB of boilerplate, but
the release artifacts must carry it.)
