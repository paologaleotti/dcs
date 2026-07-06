//! Open a file in the OS default application — the contact-sheet hand-off.
//! dcs renders the sheet to a PDF, then opens it in the platform's default
//! viewer, whose native print dialog is the real print UI (printer, paper,
//! copies, fit-to-page). Best-effort, fire-and-forget; a spawn failure is
//! ignored, exactly like [`crate::reveal`].

use std::path::Path;
use std::process::Command;

/// Open `file` in the OS default app for its type (a PDF viewer, for the
/// contact sheet). Best-effort; ignores spawn failures.
pub fn open_in_default_app(file: &Path) {
    let _ = spawn_open(file);
}

#[cfg(target_os = "macos")]
fn spawn_open(file: &Path) -> std::io::Result<std::process::Child> {
    Command::new("open").arg(file).spawn()
}

#[cfg(target_os = "windows")]
fn spawn_open(file: &Path) -> std::io::Result<std::process::Child> {
    // `start` is a `cmd` builtin; the empty "" is the (mandatory) window-title
    // argument, so a path containing spaces isn't consumed as the title.
    Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(file)
        .spawn()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_open(file: &Path) -> std::io::Result<std::process::Child> {
    Command::new("xdg-open").arg(file).spawn()
}
