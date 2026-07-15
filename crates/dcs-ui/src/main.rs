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
mod board;
mod contact_sheet;
mod context_menu;
mod crash;
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
        // basename, which cargo-packager 0.11 derives from the *binary name*
        // (`dcs.desktop`, `Icon=dcs`) — not the packager `identifier`. If they
        // drift, Linux shows a generic icon despite the icon being installed.
        .with_app_id("dcs");

    // Runtime window/dock icon for Windows, Linux, and unbundled (cargo run)
    // macOS launches. A bundled macOS app must keep eframe away from the Dock
    // icon: eframe force-sets one (ours, or its egui-logo fallback) via
    // `NSApplication.applicationIconImage`, replacing the bundle's
    // `.icns`/`Assets.car` tile and its liquid-glass treatment while the app
    // runs. There is no opt-out, but eframe validates the pixel buffer before
    // touching AppKit — a deliberately invalid IconData (empty buffer, claimed
    // 1x1) makes its setter bail out and leave the Dock tile to the bundle.
    let bundled_macos = cfg!(target_os = "macos")
        && std::env::current_exe()
            .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    let icon = if bundled_macos {
        Some(egui::IconData {
            rgba: Vec::new(),
            width: 1,
            height: 1,
        })
    } else {
        // A bad embed is a build-time bug; degrade to the platform default
        // rather than panic.
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-256.png")).ok()
    };
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("dcs", options, Box::new(|cc| Ok(Box::new(DcsApp::new(cc)))))
}
