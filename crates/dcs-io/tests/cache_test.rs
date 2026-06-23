use dcs_domain::fingerprint::ContentFingerprint;
use dcs_io::cache::{
    DEFAULT_THUMB_CAP_BYTES, EmbeddingCache, FingerprintCache, SqliteCache, ThumbCache, ThumbTier,
};

fn fp(seed: u8) -> ContentFingerprint {
    ContentFingerprint::from_bytes([seed; 32])
}

const MODEL: &str = "siglip-base-patch16-224";

#[test]
fn embedding_round_trips_exact() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    let vec = vec![0.0, 1.0, -1.5, 2.25, f32::MIN_POSITIVE];
    cache.put_embedding(&fp(1), MODEL, &vec);
    assert_eq!(cache.get_embedding(&fp(1), MODEL), Some(vec));
}

#[test]
fn embedding_miss_returns_none() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    assert_eq!(cache.get_embedding(&fp(1), MODEL), None);
}

#[test]
fn embedding_is_keyed_by_model_id() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.put_embedding(&fp(1), MODEL, &[1.0, 2.0]);
    cache.put_embedding(&fp(1), "other-model", &[9.0]);
    assert_eq!(cache.get_embedding(&fp(1), MODEL), Some(vec![1.0, 2.0]));
    assert_eq!(cache.get_embedding(&fp(1), "other-model"), Some(vec![9.0]));
}

#[test]
fn embedding_upserts_on_conflict() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.put_embedding(&fp(1), MODEL, &[1.0]);
    cache.put_embedding(&fp(1), MODEL, &[2.0, 3.0]);
    assert_eq!(cache.get_embedding(&fp(1), MODEL), Some(vec![2.0, 3.0]));
}

#[test]
fn prune_embeddings_drops_orphans_and_stale_models() {
    use std::collections::HashSet;
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.put_embedding(&fp(1), MODEL, &[1.0]);
    cache.put_embedding(&fp(2), MODEL, &[2.0]); // will be orphaned (not in keep)
    cache.put_embedding(&fp(3), MODEL, &[3.0]);
    cache.put_embedding(&fp(9), "old-model", &[9.0]); // stale model

    let keep: HashSet<_> = [fp(1), fp(3)].into_iter().collect();
    cache.prune_embeddings(MODEL, &keep);

    let mut got = cache.all_embeddings(MODEL);
    got.sort_by_key(|(f, _)| *f.as_bytes());
    assert_eq!(got, vec![(fp(1), vec![1.0]), (fp(3), vec![3.0])]);
    // The other model's rows are gone entirely.
    assert!(cache.all_embeddings("old-model").is_empty());
}

#[test]
fn all_embeddings_filters_by_model_and_returns_pairs() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.put_embedding(&fp(1), MODEL, &[1.0, 0.0]);
    cache.put_embedding(&fp(2), MODEL, &[0.0, 1.0]);
    cache.put_embedding(&fp(3), "other-model", &[5.0]);

    let mut got = cache.all_embeddings(MODEL);
    got.sort_by_key(|(f, _)| *f.as_bytes());
    assert_eq!(got, vec![(fp(1), vec![1.0, 0.0]), (fp(2), vec![0.0, 1.0])]);
    assert!(cache.all_embeddings("missing-model").is_empty());
}

#[test]
fn fingerprint_prefilter_hits_when_mtime_and_size_match() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.store("a/x.jpg", 1000, 2048, &fp(7));
    assert_eq!(cache.lookup("a/x.jpg", 1000, 2048), Some(fp(7)));
}

#[test]
fn fingerprint_prefilter_misses_when_file_changed() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.store("a/x.jpg", 1000, 2048, &fp(7));
    // mtime moved → stale, must re-hash
    assert_eq!(cache.lookup("a/x.jpg", 1001, 2048), None);
    // size moved → stale, must re-hash
    assert_eq!(cache.lookup("a/x.jpg", 1000, 4096), None);
    // never seen
    assert_eq!(cache.lookup("a/other.jpg", 1000, 2048), None);
}

#[test]
fn fingerprint_store_upserts() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.store("a/x.jpg", 1, 10, &fp(1));
    cache.store("a/x.jpg", 2, 20, &fp(2)); // same path, file changed
    assert_eq!(cache.lookup("a/x.jpg", 1, 10), None);
    assert_eq!(cache.lookup("a/x.jpg", 2, 20), Some(fp(2)));
}

#[test]
fn thumb_round_trips_per_tier() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.put(&fp(5), ThumbTier::Grid, b"grid-bytes");
    cache.put(&fp(5), ThumbTier::Gallery, b"gallery-bytes");
    assert_eq!(
        cache.get(&fp(5), ThumbTier::Grid).as_deref(),
        Some(&b"grid-bytes"[..])
    );
    assert_eq!(
        cache.get(&fp(5), ThumbTier::Gallery).as_deref(),
        Some(&b"gallery-bytes"[..])
    );
    assert_eq!(cache.get(&fp(9), ThumbTier::Grid), None);
}

#[test]
fn thumb_put_overwrites_same_key_and_tier() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.put(&fp(5), ThumbTier::Grid, b"old");
    cache.put(&fp(5), ThumbTier::Grid, b"new");
    assert_eq!(
        cache.get(&fp(5), ThumbTier::Grid).as_deref(),
        Some(&b"new"[..])
    );
}

#[test]
fn lru_evicts_oldest_when_over_cap() {
    // Cap fits two ~100-byte blobs but not three.
    let cache = SqliteCache::in_memory(250).unwrap();
    let blob = [0u8; 100];

    cache.put(&fp(1), ThumbTier::Grid, &blob);
    cache.put(&fp(2), ThumbTier::Grid, &blob);
    // Touch #1 so #2 becomes the least-recently-used.
    assert!(cache.get(&fp(1), ThumbTier::Grid).is_some());
    cache.put(&fp(3), ThumbTier::Grid, &blob); // pushes over cap → evict LRU (#2)

    assert!(
        cache.get(&fp(1), ThumbTier::Grid).is_some(),
        "recently used survives"
    );
    assert!(
        cache.get(&fp(3), ThumbTier::Grid).is_some(),
        "newest survives"
    );
    assert!(cache.get(&fp(2), ThumbTier::Grid).is_none(), "LRU evicted");
    assert!(cache.thumb_bytes() <= 250);
}

#[test]
fn staying_under_cap_evicts_nothing() {
    // The running-byte estimate must not over-evict: three small blobs well under
    // a generous cap all survive (the put hot path now skips the SUM scan).
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    let blob = [0u8; 100];
    cache.put(&fp(1), ThumbTier::Grid, &blob);
    cache.put(&fp(2), ThumbTier::Grid, &blob);
    cache.put(&fp(3), ThumbTier::Grid, &blob);

    assert!(cache.get(&fp(1), ThumbTier::Grid).is_some());
    assert!(cache.get(&fp(2), ThumbTier::Grid).is_some());
    assert!(cache.get(&fp(3), ThumbTier::Grid).is_some());
    assert_eq!(cache.thumb_bytes(), 300);
}

#[test]
fn replacing_a_blob_still_evicts_to_cap() {
    // UPSERT-replacing the same key repeatedly inflates the running estimate;
    // eviction must still keep the real total within cap (the estimate reconciles).
    let cache = SqliteCache::in_memory(250).unwrap();
    let blob = [0u8; 100];
    for _ in 0..10 {
        cache.put(&fp(1), ThumbTier::Grid, &blob); // same key, replaced each time
    }
    cache.put(&fp(2), ThumbTier::Grid, &blob);
    cache.put(&fp(3), ThumbTier::Grid, &blob);
    assert!(cache.thumb_bytes() <= 250, "real total stays within cap");
}

#[test]
fn cached_keys_lists_only_the_requested_tier() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    cache.put(&fp(1), ThumbTier::Grid, b"a");
    cache.put(&fp(2), ThumbTier::Grid, b"b");
    cache.put(&fp(2), ThumbTier::Gallery, b"c"); // same key, other tier
    cache.put(&fp(9), ThumbTier::Gallery, b"d");

    let grid = cache.cached_keys(ThumbTier::Grid);
    assert_eq!(grid.len(), 2);
    assert!(grid.contains(fp(1).as_bytes()));
    assert!(grid.contains(fp(2).as_bytes()));
    assert!(
        !grid.contains(fp(9).as_bytes()),
        "gallery-only key excluded"
    );

    let gallery = cache.cached_keys(ThumbTier::Gallery);
    assert_eq!(gallery.len(), 2);
    assert!(gallery.contains(fp(2).as_bytes()));
    assert!(gallery.contains(fp(9).as_bytes()));
}

#[test]
fn cached_keys_empty_on_fresh_cache() {
    let cache = SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap();
    assert!(cache.cached_keys(ThumbTier::Grid).is_empty());
}

#[test]
fn cache_persists_across_reopen() {
    let dir = tempdir();
    let path = dir.join("cache.sqlite3");
    {
        let cache = SqliteCache::open(&path).unwrap();
        cache.store("a/x.jpg", 1, 10, &fp(3));
        cache.put(&fp(3), ThumbTier::Grid, b"persisted");
    }
    let cache = SqliteCache::open(&path).unwrap();
    assert_eq!(cache.lookup("a/x.jpg", 1, 10), Some(fp(3)));
    assert_eq!(
        cache.get(&fp(3), ThumbTier::Grid).as_deref(),
        Some(&b"persisted"[..])
    );
}

#[test]
fn corrupt_cache_is_deleted_and_rebuilt() {
    let dir = tempdir();
    let path = dir.join("cache.sqlite3");
    // Garbage where a SQLite database should be.
    std::fs::write(&path, b"not a sqlite database at all, totally corrupt").unwrap();

    // Open must self-heal: delete the junk, rebuild, and return a usable cache.
    let cache = SqliteCache::open(&path).expect("corrupt cache rebuilds instead of failing");
    cache.store("a/x.jpg", 1, 10, &fp(4));
    assert_eq!(cache.lookup("a/x.jpg", 1, 10), Some(fp(4)));
}

/// A unique temp dir without pulling in a crate dep.
fn tempdir() -> std::path::PathBuf {
    let base = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = base.join(format!("dcs-cache-test-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
