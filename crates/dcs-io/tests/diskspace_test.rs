//! Disk-space queries against a real temp tree: free space on the filesystem
//! and a single file's size.

use std::path::PathBuf;

use dcs_io::diskspace::{available_space, file_size};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_diskspace_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn available_space_reports_a_positive_figure() {
    let dir = temp_dir("free");
    let free = available_space(&dir).expect("temp dir is on a real filesystem");
    assert!(free > 0, "a mounted filesystem has some free space");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn available_space_errors_on_a_nonexistent_path() {
    let missing = temp_dir("missing").join("does_not_exist");
    assert!(available_space(&missing).is_err());
}

#[test]
fn file_size_matches_the_written_bytes() {
    let dir = temp_dir("size");
    let file = dir.join("a.bin");
    std::fs::write(&file, vec![0u8; 4096]).unwrap();
    assert_eq!(file_size(&file), Some(4096));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_size_is_none_for_a_missing_file() {
    let missing = temp_dir("nofile").join("gone.bin");
    assert_eq!(file_size(&missing), None);
}
