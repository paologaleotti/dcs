//! dcs-ui — egui binary.
//!
//! Open-and-view slice: a folder of images rendered as a smooth, virtualized
//! contact-sheet grid over the conductor in dcs-app. Top of the dependency
//! tree.

mod app;
mod export;
mod gallery;
mod grid;
mod keymap;
mod picker;
mod theme;

use app::DcsApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_maximized(true)
            .with_title("dcs - digital contact sheet"),
        ..Default::default()
    };
    eframe::run_native("dcs", options, Box::new(|cc| Ok(Box::new(DcsApp::new(cc)))))
}
