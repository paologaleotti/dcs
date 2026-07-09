//! The no-`ai-search` build's embedder stub: construction must fail with the
//! message the UI shows, and the module's public surface must stay compiled.
#![cfg(not(feature = "ai-search"))]

use dcs_io::embedding::{MODEL_ID, SiglipEmbedder};

#[test]
fn stub_construction_fails_with_a_clear_message() {
    let Err(err) = SiglipEmbedder::new() else {
        panic!("stub must not construct");
    };
    assert!(
        err.to_string().contains("not included in this build"),
        "got: {err}"
    );
}

#[test]
fn model_id_is_stable_across_build_flavors() {
    // The cache key must not depend on whether AI search was compiled in.
    assert_eq!(MODEL_ID, "siglip-base-patch16-384-onnx");
}
