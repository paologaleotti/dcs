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
        .with_title("dcs - digital contact sheet");

    // Runtime window/taskbar icon. Decoded from the same master the packaged
    // app icons derive from; a bad embed is a build-time bug, never a panic in
    // the field, so we degrade to the platform default icon on failure.
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
