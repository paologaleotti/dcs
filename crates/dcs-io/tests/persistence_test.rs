use dcs_domain::cull::AcceptState;
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::PhotoId;
use dcs_io::persistence::{
    JsonProjectStore, PersistError, PhotoRecord, ProjectConfig, ProjectSnapshot, ProjectStore,
};

fn fp(seed: u8) -> ContentFingerprint {
    ContentFingerprint::from_bytes([seed; 32])
}

fn rec(id: u32, fp_seed: u8, verdict: AcceptState, jpeg: Option<&str>) -> PhotoRecord {
    PhotoRecord {
        id: PhotoId(id),
        fingerprint: fp(fp_seed),
        verdict,
        jpeg: jpeg.map(std::path::PathBuf::from),
        raw: None,
    }
}

fn snapshot() -> ProjectSnapshot {
    ProjectSnapshot {
        photos: vec![
            rec(0, 1, AcceptState::Accepted, Some("a.jpg")),
            rec(1, 2, AcceptState::Rejected, Some("b.jpg")),
            rec(2, 3, AcceptState::Unreviewed, Some("c.jpg")),
        ],
        next_id: 3,
        views: vec![serde_json::json!({ "kind": "Grid", "settings": { "zoom": 4 } })],
        config: ProjectConfig {
            shoot_zone: Some("Europe/Rome".to_string()),
            camera_zone: Some("Asia/Tokyo".to_string()),
            grid_zoom: Some(180.0),
        },
    }
}

#[test]
fn round_trips_through_disk() {
    let dir = tempdir();
    let store = JsonProjectStore;
    let original = snapshot();
    store.save(&dir, &original).unwrap();
    let loaded = store.load(&dir).unwrap().unwrap();
    assert_eq!(loaded, original);
}

#[test]
fn missing_project_loads_as_none() {
    let dir = tempdir();
    assert!(JsonProjectStore.load(&dir).unwrap().is_none());
}

#[test]
fn unknown_view_kind_is_preserved_verbatim() {
    let dir = tempdir();
    let store = JsonProjectStore;
    let mut snap = snapshot();
    // A future ViewKind this build has never heard of.
    snap.views.push(serde_json::json!({
        "kind": "Board",
        "members": [1, 2, 3],
        "positions": { "1": [10, 20] }
    }));
    store.save(&dir, &snap).unwrap();
    let loaded = store.load(&dir).unwrap().unwrap();
    assert_eq!(
        loaded.views, snap.views,
        "unknown kinds round-trip untouched"
    );
}

#[test]
fn unreviewed_photos_are_persisted_for_id_reclaim() {
    // All known photos must be stored, even unreviewed, so a renamed unreviewed
    // photo still reclaims its id.
    let dir = tempdir();
    let store = JsonProjectStore;
    store.save(&dir, &snapshot()).unwrap();
    let loaded = store.load(&dir).unwrap().unwrap();
    assert!(
        loaded
            .photos
            .iter()
            .any(|p| p.verdict == AcceptState::Unreviewed)
    );
    assert_eq!(loaded.seed_map().get(&fp(3)), Some(&PhotoId(2)));
}

#[test]
fn config_and_paths_round_trip() {
    let dir = tempdir();
    let store = JsonProjectStore;
    store.save(&dir, &snapshot()).unwrap();
    let loaded = store.load(&dir).unwrap().unwrap();
    assert_eq!(loaded.config.shoot_zone.as_deref(), Some("Europe/Rome"));
    assert_eq!(loaded.config.grid_zoom, Some(180.0));
    assert_eq!(
        loaded.photos[0].jpeg,
        Some(std::path::PathBuf::from("a.jpg"))
    );
}

#[test]
fn old_file_without_config_or_paths_loads_with_defaults() {
    // A v1 file written before config/paths existed must still load.
    let dir = tempdir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("project.json"),
        r#"{"version":1,"photos":[{"id":0,"fingerprint":"aa","verdict":"Accepted"}],"next_id":1,"views":[]}"#
            .replace("\"aa\"", &format!("\"{}\"", "aa".repeat(32))),
    )
    .unwrap();
    let loaded = JsonProjectStore.load(&dir).unwrap().unwrap();
    assert_eq!(loaded.config, ProjectConfig::default());
    assert_eq!(loaded.photos[0].jpeg, None);
    assert_eq!(loaded.photos[0].verdict, AcceptState::Accepted);
}

#[test]
fn newer_version_is_refused_not_guessed() {
    let dir = tempdir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("project.json"),
        r#"{"version":999,"photos":[],"next_id":0,"views":[]}"#,
    )
    .unwrap();
    match JsonProjectStore.load(&dir) {
        Err(PersistError::UnsupportedVersion(999)) => {}
        other => panic!("expected UnsupportedVersion(999), got {other:?}"),
    }
}

#[test]
fn save_rotates_a_backup_and_leaves_no_temp() {
    let dir = tempdir();
    let store = JsonProjectStore;

    let mut first = snapshot();
    first.next_id = 10;
    store.save(&dir, &first).unwrap();
    // No backup yet on the first write — nothing to rotate.
    assert!(!dir.join("project.json.bak").exists());

    let mut second = snapshot();
    second.next_id = 20;
    store.save(&dir, &second).unwrap();

    // The backup now holds the prior (first) version; the main holds the new one.
    assert!(dir.join("project.json.bak").exists());
    assert!(
        !dir.join("project.json.tmp").exists(),
        "temp must not linger"
    );
    assert_eq!(store.load(&dir).unwrap().unwrap().next_id, 20);
}

#[test]
fn load_falls_back_to_backup_when_main_is_torn() {
    let dir = tempdir();
    let store = JsonProjectStore;
    store.save(&dir, &snapshot()).unwrap(); // writes main
    store.save(&dir, &snapshot()).unwrap(); // now a valid .bak exists

    // Simulate a torn main file (crash mid-write left garbage).
    std::fs::write(dir.join("project.json"), b"{ this is not json").unwrap();

    let loaded = store.load(&dir).unwrap().unwrap();
    assert_eq!(loaded, snapshot(), "recovered from the backup");
}

fn tempdir() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir()
        .join(format!(
            "dcs-persist-test-{nanos}-{:?}",
            std::thread::current().id()
        ))
        .join(".dcs");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
