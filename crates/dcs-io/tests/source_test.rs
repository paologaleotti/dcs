use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dcs_domain::pairing::{FileKind, ScannedFile};
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

/// An unreadable file must fall back to a path-derived identity and must NOT
/// cache it: once readable again, the real content hash takes over — a cached
/// wrong fingerprint would permanently detach the photo from its verdicts.
#[cfg(unix)]
#[test]
fn unreadable_file_gets_uncached_path_identity() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir();
    let path = dir.join("locked.jpg");
    write(&path, b"secret bytes");
    let readable_fp = scan_all(&dir, None)[0].fingerprint;

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let cache = Arc::new(Mutex::new(
        SqliteCache::in_memory(DEFAULT_THUMB_CAP_BYTES).unwrap(),
    ));
    let locked = scan_all(&dir, Some(Arc::clone(&cache)));
    let locked_fp = locked[0].fingerprint;
    assert_ne!(
        locked_fp, readable_fp,
        "unreadable file falls back to path identity"
    );

    // The fallback identity is never cached, so the next scan (readable again)
    // recomputes the true content hash instead of serving the fallback.
    {
        let guard = cache.lock().unwrap();
        use dcs_io::cache::FingerprintCache;
        let meta = std::fs::metadata(&path).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(
            guard.lookup("locked.jpg", mtime, meta.len()),
            None,
            "fallback fingerprint must not be cached"
        );
    }

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let restored = scan_all(&dir, Some(cache));
    assert_eq!(
        restored[0].fingerprint, readable_fp,
        "content identity returns once the file is readable"
    );
}

/// A minimal EXIF-bearing JPEG carrying `DateTimeOriginal`. Written under a RAW
/// extension below: `classify` keys on the extension while the EXIF reader keys
/// on the container, so this exercises the scan's RAW branch without needing a
/// real camera file.
fn exif_image(dt: &str) -> Vec<u8> {
    assert_eq!(dt.len(), 19, "DateTimeOriginal must be YYYY:MM:DD HH:MM:SS");
    let mut img = image::RgbImage::new(16, 16);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([x as u8, y as u8, 200]);
    }
    let mut jpeg = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut jpeg),
        image::ImageFormat::Jpeg,
    )
    .expect("encode jpeg");

    // TIFF block: header → IFD0 (one Exif-IFD pointer) → Exif IFD (the time tag)
    // → the DateTimeOriginal string. Offsets are relative to the header.
    let mut tiff: Vec<u8> = Vec::new();
    tiff.extend(b"II");
    tiff.extend(42u16.to_le_bytes());
    tiff.extend(8u32.to_le_bytes());
    tiff.extend(1u16.to_le_bytes());
    tiff.extend(0x8769u16.to_le_bytes());
    tiff.extend(4u16.to_le_bytes());
    tiff.extend(1u32.to_le_bytes());
    tiff.extend(26u32.to_le_bytes());
    tiff.extend(0u32.to_le_bytes());
    tiff.extend(1u16.to_le_bytes());
    tiff.extend(0x9003u16.to_le_bytes());
    tiff.extend(2u16.to_le_bytes());
    tiff.extend(20u32.to_le_bytes());
    tiff.extend(44u32.to_le_bytes());
    tiff.extend(0u32.to_le_bytes());
    assert_eq!(tiff.len(), 44);
    tiff.extend(dt.as_bytes());
    tiff.push(0);

    let mut app1 = Vec::new();
    app1.extend(b"Exif\0\0");
    app1.extend(&tiff);
    let seg_len = (app1.len() + 2) as u16;

    let mut out = Vec::new();
    out.extend(&jpeg[0..2]);
    out.extend([0xFF, 0xE1]);
    out.extend(seg_len.to_be_bytes());
    out.extend(&app1);
    out.extend(&jpeg[2..]);
    out
}

#[test]
fn a_raw_file_is_read_like_a_jpeg() {
    let dir = tempdir();
    write(&dir.join("shot.nef"), &exif_image("2024:05:04 09:30:00"));

    let files = scan_all(&dir, None);

    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0].kind,
        FileKind::Raw,
        "the extension decides the kind"
    );
    // A shown RAW has to group and sort by when it was taken, so the scan reads
    // its capture time rather than leaving it undated.
    assert!(
        files[0].captured_at.is_some(),
        "a RAW's capture time is read"
    );
}

#[test]
fn renaming_a_raw_keeps_its_fingerprint() {
    let dir = tempdir();
    let original = dir.join("DSCF9.nef");
    write(&original, b"raw bytes that do not change");
    let before = scan_all(&dir, None)[0].fingerprint;

    std::fs::rename(&original, dir.join("KEEPER.nef")).unwrap();
    let after = scan_all(&dir, None)[0].fingerprint;

    // RAW-only photos own verdicts and tags now, so their identity has to be the
    // content — a path-derived one would drop that state on a rename.
    assert_eq!(before, after, "content identity survives a rename");
}

/// A bare TIFF block holding `DateTimeOriginal` — the shape of EXIF outside a
/// JPEG segment, as CR3's maker boxes store it.
fn tiff_exif(dt: &str) -> Vec<u8> {
    assert_eq!(dt.len(), 19, "DateTimeOriginal must be YYYY:MM:DD HH:MM:SS");
    let mut t: Vec<u8> = Vec::new();
    t.extend(b"II");
    t.extend(42u16.to_le_bytes());
    t.extend(8u32.to_le_bytes());
    t.extend(1u16.to_le_bytes());
    t.extend(0x8769u16.to_le_bytes()); // Exif IFD pointer → 26
    t.extend(4u16.to_le_bytes());
    t.extend(1u32.to_le_bytes());
    t.extend(26u32.to_le_bytes());
    t.extend(0u32.to_le_bytes());
    t.extend(1u16.to_le_bytes());
    t.extend(0x9003u16.to_le_bytes()); // DateTimeOriginal ASCII[20] → 44
    t.extend(2u16.to_le_bytes());
    t.extend(20u32.to_le_bytes());
    t.extend(44u32.to_le_bytes());
    t.extend(0u32.to_le_bytes());
    assert_eq!(t.len(), 44);
    t.extend(dt.as_bytes());
    t.push(0);
    t
}

/// A bare TIFF block with orientation but no capture time — CR3's `CMT1`, which
/// the search must step over to reach the box that has the time.
fn tiff_exif_without_time() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::new();
    t.extend(b"II");
    t.extend(42u16.to_le_bytes());
    t.extend(8u32.to_le_bytes());
    t.extend(1u16.to_le_bytes());
    t.extend(0x0112u16.to_le_bytes()); // Orientation, SHORT, inline
    t.extend(3u16.to_le_bytes());
    t.extend(1u32.to_le_bytes());
    t.extend(1u32.to_le_bytes());
    t.extend(0u32.to_le_bytes());
    t
}

/// A RAF: Fuji's magic, then the header's stated preview offset/length, then the
/// preview JPEG — whose APP1 block is the only EXIF in the file. The container
/// itself is not TIFF, so a sniffing EXIF reader rejects it outright.
#[test]
fn a_raf_gets_its_date_from_the_embedded_preview() {
    let dir = tempdir();
    let preview = exif_image("2023:11:02 17:45:10");
    let offset: u32 = 148;
    let mut data = Vec::new();
    data.extend_from_slice(b"FUJIFILMCCD-RAW ");
    data.resize(84, 0);
    data.extend_from_slice(&offset.to_be_bytes());
    data.extend_from_slice(&(preview.len() as u32).to_be_bytes());
    data.resize(offset as usize, 0);
    data.extend_from_slice(&preview);
    write(&dir.join("DSCF1.raf"), &data);

    let files = scan_all(&dir, None);

    assert_eq!(files.len(), 1);
    assert!(
        files[0].captured_at.is_some(),
        "a RAF must not import undated — it would group and sort wrong"
    );
}

/// A CR3: ISO-BMFF, so the EXIF lives in `CMT1`/`CMT2` boxes rather than a JPEG
/// segment. `CMT2` is the one holding the capture time.
#[test]
fn a_cr3_gets_its_date_from_its_exif_box() {
    let dir = tempdir();
    let tiff = tiff_exif("2022:07:19 06:05:00");
    let mut data = Vec::new();
    data.extend_from_slice(&[0, 0, 0, 24]);
    data.extend_from_slice(b"ftypcrx crx ");
    // A CMT1 box with no capture time, so the search has to keep going.
    let bare = tiff_exif_without_time();
    data.extend_from_slice(&((bare.len() + 8) as u32).to_be_bytes());
    data.extend_from_slice(b"CMT1");
    data.extend_from_slice(&bare);
    data.extend_from_slice(&((tiff.len() + 8) as u32).to_be_bytes());
    data.extend_from_slice(b"CMT2");
    data.extend_from_slice(&tiff);
    write(&dir.join("IMG_1.cr3"), &data);

    let files = scan_all(&dir, None);

    assert_eq!(files.len(), 1);
    assert!(
        files[0].captured_at.is_some(),
        "a CR3's capture time is in a maker box, not a JPEG segment"
    );
}

#[test]
fn a_file_with_no_exif_date_falls_back_to_its_mtime() {
    let dir = tempdir();
    // A plain JPEG with no EXIF block at all.
    write(&dir.join("scan.jpg"), &{
        let mut img = image::RgbImage::new(8, 8);
        img.enumerate_pixels_mut()
            .for_each(|(x, y, px)| *px = image::Rgb([x as u8, y as u8, 7]));
        let mut out = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        out
    });

    let files = scan_all(&dir, None);

    assert_eq!(files.len(), 1);
    let f = &files[0];
    assert!(
        f.captured_at.is_some(),
        "the file time stands in so the photo is not exiled to No date"
    );
    assert!(f.captured_approx, "and it is marked approximate");
    assert_eq!(
        f.captured_offset,
        Some(time::UtcOffset::UTC),
        "an mtime is an absolute instant, so it carries UTC, not a camera zone"
    );
    // The fallback tracks the actual file time (same day is close enough for a
    // just-written file, whatever the local zone).
    let now = time::OffsetDateTime::now_utc();
    assert_eq!(f.captured_at.unwrap().date(), now.date());
}

#[test]
fn a_real_exif_date_is_never_marked_approximate() {
    let dir = tempdir();
    write(&dir.join("dated.jpg"), &exif_image("2024:05:04 09:30:00"));

    let files = scan_all(&dir, None);

    assert!(!files[0].captured_approx);
    assert_eq!(files[0].captured_at.unwrap().year(), 2024);
}
