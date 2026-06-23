use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dcs_domain::pairing::ScannedFile;
use dcs_io::cache::{DEFAULT_THUMB_CAP_BYTES, SqliteCache};
use dcs_io::source::scan;

/// Run a scan to completion and collect every file it streamed.
fn scan_all(root: &Path, cache: Option<Arc<Mutex<SqliteCache>>>) -> Vec<ScannedFile> {
    let handle = scan(root.to_path_buf(), cache);
    let mut out = Vec::new();
    while handle.is_running() {
        out.extend(handle.drain());
        std::thread::yield_now();
    }
    out.extend(handle.drain());
    out
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn identical_content_under_different_names_shares_a_fingerprint() {
    let dir = tempdir();
    write(&dir.join("a.jpg"), b"the same pixels");
    write(&dir.join("b.jpg"), b"the same pixels");
    write(&dir.join("c.jpg"), b"different pixels entirely");

    let files = scan_all(&dir, None);
    let fp = |name: &str| {
        files
            .iter()
            .find(|f| f.path.file_name().unwrap() == name)
            .unwrap()
            .fingerprint
    };
    assert_eq!(fp("a.jpg"), fp("b.jpg"), "same content => same fingerprint");
    assert_ne!(fp("a.jpg"), fp("c.jpg"), "different content => different");
}

#[test]
fn rename_in_place_keeps_the_fingerprint() {
    let dir = tempdir();
    let original = dir.join("DSCF1.jpg");
    write(&original, b"burst frame 7 bytes");
    let before = scan_all(&dir, None)[0].fingerprint;

    std::fs::rename(&original, dir.join("KEEPER.jpg")).unwrap();
    let after = scan_all(&dir, None)[0].fingerprint;

    assert_eq!(before, after, "content identity survives a rename");
}

#[test]
fn large_file_uses_head_tail_and_size() {
    let dir = tempdir();
    // > 2 * 64K so the head+tail path runs; differ only in the middle.
    let mut a = vec![0u8; 300 * 1024];
    let mut b = a.clone();
    a[150 * 1024] = 1;
    b[150 * 1024] = 2;
    write(&dir.join("a.jpg"), &a);
    write(&dir.join("b.jpg"), &b);

    let files = scan_all(&dir, None);
    let fp_a = files
        .iter()
        .find(|f| f.path.ends_with("a.jpg"))
        .unwrap()
        .fingerprint;
    let fp_b = files
        .iter()
        .find(|f| f.path.ends_with("b.jpg"))
        .unwrap()
        .fingerprint;
    // Head, tail, and size all match; only the unhashed middle differs, so the
    // head+tail strategy treats them as identical (documented trade-off, #33).
    assert_eq!(fp_a, fp_b);
}

#[test]
fn cache_prefilter_records_fingerprints() {
    let dir = tempdir();
    write(&dir.join("a.jpg"), b"cache me");
    let cache = Arc::new(Mutex::new(
        SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap(),
    ));

    let first = scan_all(&dir, Some(Arc::clone(&cache)));
    let fp_first = first[0].fingerprint;

    // Second scan must reproduce the same fingerprint (served from cache when
    // (mtime,size) are unchanged, recomputed otherwise — same result either way).
    let second = scan_all(&dir, Some(Arc::clone(&cache)));
    assert_eq!(second[0].fingerprint, fp_first);

    // The cache holds the entry under the relative path.
    let guard = cache.lock().unwrap();
    use dcs_io::cache::FingerprintCache;
    let meta = std::fs::metadata(dir.join("a.jpg")).unwrap();
    let mtime = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(guard.lookup("a.jpg", mtime, meta.len()), Some(fp_first));
}

#[test]
fn dot_named_root_still_scans_its_files() {
    // A root folder whose own name starts with a dot (e.g. dragging in
    // `.archive`) is a legitimate import: the hidden filter must skip the root
    // and only prune descendants, or the whole walk returns empty.
    let parent = tempdir();
    let root = parent.join(".archive");
    std::fs::create_dir_all(&root).unwrap();
    write(&root.join("a.jpg"), b"inside a dot root");

    let files = scan_all(&root, None);
    assert_eq!(
        files.len(),
        1,
        "a dot-named root must still surface its files"
    );
    assert!(files[0].path.ends_with("a.jpg"));
}

#[test]
fn hidden_descendants_are_still_pruned() {
    let dir = tempdir();
    write(&dir.join("visible.jpg"), b"shown");
    let hidden = dir.join(".dcs");
    std::fs::create_dir_all(&hidden).unwrap();
    write(&hidden.join("cached.jpg"), b"sidecar junk");

    let files = scan_all(&dir, None);
    assert_eq!(files.len(), 1, "the .dcs sidecar and dotfiles stay pruned");
    assert!(files[0].path.ends_with("visible.jpg"));
}

fn tempdir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Thread id keeps parallel test cases from colliding on the same folder.
    let dir = std::env::temp_dir().join(format!(
        "dcs-source-test-{nanos}-{:?}",
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
