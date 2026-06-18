//! Open the OS file manager at a path — the affordance that keeps "dcs
//! never deletes" from being a dead end: reveal the rejected originals so the
//! user can act on them outside the app. Best-effort; a spawn failure is ignored.

use std::path::Path;
use std::process::Command;

/// Open the platform file manager showing the folder `path`.
pub fn reveal(path: &Path) {
    let _ = spawn(path);
}

/// Open the file manager with `file` selected/highlighted where the platform
/// supports it, else its containing folder.
pub fn reveal_file(file: &Path) {
    let _ = spawn_select(file);
}

#[cfg(target_os = "macos")]
fn spawn(path: &Path) -> std::io::Result<std::process::Child> {
    Command::new("open").arg(path).spawn()
}

#[cfg(target_os = "windows")]
fn spawn(path: &Path) -> std::io::Result<std::process::Child> {
    Command::new("explorer").arg(path).spawn()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn(path: &Path) -> std::io::Result<std::process::Child> {
    Command::new("xdg-open").arg(path).spawn()
}

#[cfg(target_os = "macos")]
fn spawn_select(file: &Path) -> std::io::Result<std::process::Child> {
    Command::new("open").arg("-R").arg(file).spawn()
}

#[cfg(target_os = "windows")]
fn spawn_select(file: &Path) -> std::io::Result<std::process::Child> {
    Command::new("explorer")
        .arg(format!("/select,{}", file.display()))
        .spawn()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_select(file: &Path) -> std::io::Result<std::process::Child> {
    // No portable "select" on Linux file managers; open the containing folder.
    let dir = file.parent().unwrap_or(file);
    Command::new("xdg-open").arg(dir).spawn()
}
