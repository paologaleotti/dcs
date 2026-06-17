//! The keyboard half of the command registry. One table maps key
//! chords to [`AppAction`]s; it drives both directions — resolving pressed keys
//! into actions, and rendering the key hint shown beside each palette row — so
//! the two can never drift. Adding a binding is one row here.
//!
//! `Modifiers::command` is egui's portable primary modifier: ⌘ on macOS, Ctrl
//! on Windows/Linux, so the same table is correct on every platform.

use dcs_app::AppAction;
use egui::Key;

/// Resolve this frame's pressed keys into command actions (geometry keys like
/// arrows are handled separately by the grid, not here).
pub fn actions_for_input(ctx: &egui::Context) -> Vec<AppAction> {
    ctx.input(|i| {
        KEYMAP
            .iter()
            .filter(|(_, chord)| {
                i.key_pressed(chord.key)
                    && i.modifiers.command == chord.cmd
                    && i.modifiers.shift == chord.shift
            })
            .map(|(action, _)| *action)
            .collect()
    })
}

/// The key hint for an action (e.g. `⌘Z` / `Ctrl+Z`), or `None` if unbound.
/// Uses the first chord bound to the action.
pub fn hint(action: AppAction) -> Option<String> {
    let (_, chord) = KEYMAP.iter().find(|(a, _)| *a == action)?;
    let mut s = String::new();
    if chord.cmd {
        s.push_str(MOD);
    }
    if chord.shift {
        s.push_str(SHIFT);
    }
    s.push_str(key_label(chord.key));
    Some(s)
}

struct Chord {
    key: Key,
    cmd: bool,
    shift: bool,
}

const fn plain(key: Key) -> Chord {
    Chord {
        key,
        cmd: false,
        shift: false,
    }
}

const fn cmd(key: Key) -> Chord {
    Chord {
        key,
        cmd: true,
        shift: false,
    }
}

const fn cmd_shift(key: Key) -> Chord {
    Chord {
        key,
        cmd: true,
        shift: true,
    }
}

/// The single source of key bindings. Order matters only for `hint` (first
/// match wins), so list the canonical chord for an action first.
const KEYMAP: &[(AppAction, Chord)] = &[
    (AppAction::Accept, plain(Key::A)),
    (AppAction::Reject, plain(Key::X)),
    (AppAction::ShowMetadata, plain(Key::I)),
    (AppAction::Undo, cmd(Key::Z)),
    (AppAction::Redo, cmd_shift(Key::Z)),
    (AppAction::OpenFolder, cmd(Key::O)),
    (AppAction::ZoomIn, plain(Key::Plus)),
    (AppAction::ZoomIn, plain(Key::Equals)),
    (AppAction::ZoomOut, plain(Key::Minus)),
    (AppAction::ToggleDiagnostics, plain(Key::F12)),
    (AppAction::Quit, cmd(Key::Q)),
];

#[cfg(target_os = "macos")]
const MOD: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const MOD: &str = "Ctrl+";

#[cfg(target_os = "macos")]
const SHIFT: &str = "⇧";
#[cfg(not(target_os = "macos"))]
const SHIFT: &str = "Shift+";

fn key_label(key: Key) -> &'static str {
    match key {
        Key::Plus | Key::Equals => "+",
        Key::Minus => "−",
        Key::Num0 => "0",
        Key::F12 => "F12",
        other => other.name(),
    }
}
