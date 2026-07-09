//! Graceful handling of a main-thread panic. Without this an `unwrap` deep in a
//! frame would tear the process down with nothing the user can act on. Instead a
//! global panic hook records the panic (message, location, backtrace); the frame
//! loop catches the unwind and, from then on, paints a crash screen with a
//! copiable report so the user can file it and restart. Worker-thread panics
//! stay handled where they occur (the export executor catches its own) — this is
//! only the last resort for the UI thread.

use std::backtrace::Backtrace;
use std::panic::PanicHookInfo;
use std::sync::{Mutex, OnceLock};

use egui::{RichText, Ui};

use crate::theme;

/// A captured main-thread panic, latched onto the app and rendered until quit.
#[derive(Clone)]
pub struct CrashReport {
    message: String,
    location: String,
    backtrace: String,
}

impl CrashReport {
    /// Fallback when a panic was caught but the hook left no details (should not
    /// happen — the hook runs before the unwind reaches the catch).
    pub fn unknown() -> Self {
        CrashReport {
            message: "panic with no captured details".to_string(),
            location: "unknown location".to_string(),
            backtrace: "backtrace unavailable".to_string(),
        }
    }

    /// The full report, copied verbatim into a bug report: version, message,
    /// location, platform, then the backtrace.
    pub fn report(&self) -> String {
        format!(
            "dcs v{} — panic\n{}\nat {}\n{} {}\n\n{}",
            env!("CARGO_PKG_VERSION"),
            self.message,
            self.location,
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.backtrace,
        )
    }
}

/// Install the panic hook once, chaining the previous one so terminal/log output
/// still appears. Idempotent — safe to call from every app construction.
pub fn install_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let report = CrashReport {
                message: payload_message(info),
                location: info
                    .location()
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "unknown location".to_string()),
                backtrace: Backtrace::force_capture().to_string(),
            };
            if let Ok(mut slot) = LAST_PANIC.lock() {
                *slot = Some(report);
            }
            previous(info);
        }));
    });
}

/// Take the most recently captured panic, if any. The frame loop calls this only
/// after its own `catch_unwind` reports an unwind, so a stale report left by a
/// worker panic (which is handled elsewhere) is never surfaced on its own.
pub fn take_report() -> Option<CrashReport> {
    LAST_PANIC.lock().ok().and_then(|mut slot| slot.take())
}

/// Paint the crash screen: the panic headline, a scrollable full report, and the
/// two actions — copy the details, or quit. Never touches the crashed app state.
pub fn show(ui: &mut Ui, report: &CrashReport) {
    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, 0.0, theme::EXTREME);

    let text = report.report();
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(area.shrink2(egui::Vec2::new(40.0, 32.0))));
    child.vertical(|ui| {
        ui.add_space(8.0);
        ui.label(
            RichText::new("dcs stopped unexpectedly")
                .strong()
                .size(20.0)
                .color(theme::VERDICT_REJECT),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(&report.message)
                .monospace()
                .color(theme::TEXT_HOVER),
        );
        ui.label(
            RichText::new(format!("at {}", report.location))
                .monospace()
                .small()
                .color(theme::TEXT_DIM),
        );

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let copied_id = egui::Id::new("crash_copied");
            if ui.button("Copy details").clicked() {
                ui.ctx().copy_text(text.clone());
                ui.data_mut(|d| d.insert_temp(copied_id, true));
            }
            if ui.data(|d| d.get_temp::<bool>(copied_id).unwrap_or(false)) {
                ui.label(
                    RichText::new("copied ✓")
                        .small()
                        .color(theme::VERDICT_ACCEPT),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Quit dcs").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Please copy the details and report at github.com/paologaleotti/dcs/issues.",
            )
            .small()
            .color(theme::TEXT_DIM),
        );

        ui.add_space(10.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // egui treats a `&str` as a read-only TextBuffer: selectable and
                // copiable, but keystrokes are dropped — so this is a scrollable,
                // selectable report, not an editable field.
                ui.add(
                    egui::TextEdit::multiline(&mut text.as_str())
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .code_editor(),
                );
            });
    });
}

static LAST_PANIC: Mutex<Option<CrashReport>> = Mutex::new(None);

fn payload_message(info: &PanicHookInfo) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unrecognized panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_carries_message_location_and_backtrace() {
        let r = CrashReport {
            message: "boom".to_string(),
            location: "src/x.rs:12:3".to_string(),
            backtrace: "0: frame".to_string(),
        };
        let text = r.report();
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        assert!(text.contains("boom"), "{text}");
        assert!(text.contains("at src/x.rs:12:3"), "{text}");
        assert!(text.contains(std::env::consts::OS), "{text}");
        assert!(text.contains("0: frame"), "{text}");
    }

    #[test]
    fn unknown_report_is_nonempty_and_formats() {
        let text = CrashReport::unknown().report();
        assert!(text.contains("panic with no captured details"), "{text}");
        assert!(text.contains("dcs v"), "{text}");
    }
}
