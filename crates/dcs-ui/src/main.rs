//! dcs-ui — egui binary.
//!
//! Open-and-view slice: a folder of images rendered as a smooth, virtualized
//! contact-sheet grid over the conductor in dcs-app. Top of the dependency
//! tree.

// Release builds on Windows are GUI apps: suppress the console window the
// default console subsystem spawns behind the egui window. Debug keeps it so
// panic/`eprintln!` output stays visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod context_menu;
mod crop;
mod export;
mod gallery;
mod grid;
mod keymap;
mod picker;
mod theme;

use app::DcsApp;

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1200.0, 800.0])
        .with_maximized(true)
        .with_title("dcs - digital contact sheet")
        // Wayland/X11 associate a window with its installed `.desktop` entry (and
        // thus its taskbar/dock icon) by app_id. It must equal the desktop file
        // basename, which cargo-packager derives from the packager `identifier`.
        // If they drift, Linux shows a generic icon despite the icon being
        // installed.
        .with_app_id("io.github.paologaleotti.dcs");

    // Runtime window/dock icon, on every platform — without it macOS dev runs and
    // any unbundled launch fall back to a generic placeholder. The PNG carries the
    // same padded squircle the `.icns` does (one shared SVG master), so the
    // runtime dock tile matches the bundle icon's size and shape. A bad embed is a
    // build-time bug, so we degrade to the platform default rather than panic.
    if let Ok(icon) =
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-256.png"))
    {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("dcs", options, Box::new(|cc| Ok(Box::new(DcsApp::new(cc)))))
}
