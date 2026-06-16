//! Open the OS file manager at a path (§6.5) — the affordance that keeps "dcs
//! never deletes" from being a dead end: reveal the rejected originals so the
//! user can act on them outside the app. Best-effort; a spawn failure is ignored.

use std::path::Path;
use std::process::Command;

/// Open the platform file manager showing `path`.
pub fn reveal(path: &Path) {
    let _ = spawn(path);
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
