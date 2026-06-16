use std::path::PathBuf;

use dcs_io::recents::{self, MAX_RECENTS, Recents};

#[test]
fn record_moves_to_front_and_dedups() {
    let mut r = Recents::default();
    r.record(PathBuf::from("/a"));
    r.record(PathBuf::from("/b"));
    r.record(PathBuf::from("/a")); // re-open /a → back to front, no duplicate
    assert_eq!(r.projects, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
}

#[test]
fn record_caps_at_max() {
    let mut r = Recents::default();
    for i in 0..(MAX_RECENTS + 5) {
        r.record(PathBuf::from(format!("/p{i}")));
    }
    assert_eq!(r.projects.len(), MAX_RECENTS);
    // The most-recent insert is first; the oldest were dropped.
    assert_eq!(r.projects[0], PathBuf::from(format!("/p{}", MAX_RECENTS + 4)));
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempdir();
    let path = dir.join("recents.json");
    let mut r = Recents::default();
    r.record(PathBuf::from("/trips/japan"));
    r.record(PathBuf::from("/trips/iceland"));
    recents::save(&path, &r).unwrap();
    assert_eq!(recents::load(&path), r);
}

#[test]
fn load_missing_or_corrupt_yields_empty() {
    let dir = tempdir();
    assert_eq!(recents::load(&dir.join("nope.json")), Recents::default());
    let corrupt = dir.join("bad.json");
    std::fs::write(&corrupt, b"{ not json").unwrap();
    assert_eq!(recents::load(&corrupt), Recents::default());
}

#[test]
fn retain_existing_drops_dead_paths() {
    let dir = tempdir();
    let alive = dir.join("alive");
    std::fs::create_dir_all(&alive).unwrap();
    let dead = dir.join("deleted");

    let mut r = Recents::default();
    r.record(dead.clone());
    r.record(alive.clone());
    r.retain_existing();

    assert!(r.projects.contains(&alive), "existing folder kept");
    assert!(!r.projects.contains(&dead), "nonexistent folder pruned");
}

fn tempdir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dcs-recents-{nanos}-{:?}", std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
