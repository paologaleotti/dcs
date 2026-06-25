//! The keyboard half of the command registry. One table ([`KEYMAP`]) maps key
//! chords to [`AppAction`]s plus a human description, and drives all three
//! readers — resolving pressed keys into actions, the key hint beside each
//! palette row, and the Help shortcuts reference ([`shortcuts`]) — so they can
//! never drift. Adding a binding is one row here.
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
            .filter(|(_, chord, _)| {
                i.key_pressed(chord.key)
                    && i.modifiers.command == chord.cmd
                    && i.modifiers.shift == chord.shift
            })
            .map(|(action, _, _)| *action)
            .collect()
    })
}

/// The key hint for an action (e.g. `⌘Z` / `Ctrl+Z`), or `None` if unbound.
/// Uses the first chord bound to the action.
pub fn hint(action: AppAction) -> Option<String> {
    let (_, chord, _) = KEYMAP.iter().find(|(a, _, _)| *a == action)?;
    Some(render_chord(chord))
}

/// One row of the keyboard-shortcuts reference: a rendered chord and what it
/// does. Built from the one [`KEYMAP`] source, so the reference can never drift
/// from the live bindings.
pub struct Shortcut {
    pub keys: String,
    pub description: &'static str,
}

/// Every shortcut for the Help reference. The remappable, registry-dispatched
/// bindings come from [`KEYMAP`] (one row per action, alias chords folded in);
/// the trailing rows are the view/navigation keys the grid and gallery read
/// directly — fixed for now, listed here so the reference stays complete.
pub fn shortcuts() -> Vec<Shortcut> {
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (action, chord, description) in KEYMAP {
        if seen.insert(action.id()) {
            rows.push(Shortcut {
                keys: render_chord(chord),
                description,
            });
        }
    }
    rows.push(Shortcut {
        keys: format!("{MOD}P"),
        description: "Open command palette",
    });
    rows.push(Shortcut {
        keys: "?".to_string(),
        description: "Keyboard shortcuts",
    });
    rows.push(Shortcut {
        keys: "↑ ↓ ← →".to_string(),
        description: "Move focus",
    });
    rows.push(Shortcut {
        keys: format!("{SHIFT}↑ ↓ ← →"),
        description: "Extend selection",
    });
    rows.push(Shortcut {
        keys: "Z".to_string(),
        description: "Gallery: toggle 1:1 zoom",
    });
    rows.push(Shortcut {
        keys: "Esc".to_string(),
        description: "Clear selection / close",
    });
    rows
}

fn render_chord(chord: &Chord) -> String {
    let mut s = String::new();
    if chord.cmd {
        s.push_str(MOD);
    }
    if chord.shift {
        s.push_str(SHIFT);
    }
    s.push_str(key_label(chord.key));
    s
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

const fn shift(key: Key) -> Chord {
    Chord {
        key,
        cmd: false,
        shift: true,
    }
}

const fn cmd_shift(key: Key) -> Chord {
    Chord {
        key,
        cmd: true,
        shift: true,
    }
}

/// The single source of key bindings — drives dispatch, the palette hints, and
/// the Help shortcuts reference. Each row carries a human description for that
/// reference. Order matters only for `hint`/`shortcuts` (first match wins, alias
/// chords folded in), so list the canonical chord for an action first.
const KEYMAP: &[(AppAction, Chord, &str)] = &[
    (AppAction::Accept, plain(Key::A), "Accept"),
    (AppAction::Reject, plain(Key::X), "Reject"),
    (
        AppAction::ToggleGallery,
        plain(Key::Space),
        "Open / close gallery",
    ),
    (AppAction::SelectAll, cmd(Key::A), "Select all"),
    (AppAction::OpenTagPalette, plain(Key::T), "Add tag"),
    (AppAction::OpenUntagPalette, shift(Key::T), "Remove tag"),
    (AppAction::ShowMetadata, plain(Key::I), "Photo info"),
    (AppAction::EnterCrop, plain(Key::C), "Crop photo"),
    (AppAction::Undo, cmd(Key::Z), "Undo"),
    (AppAction::Redo, cmd_shift(Key::Z), "Redo"),
    (AppAction::OpenFolder, cmd(Key::O), "Open folder"),
    (AppAction::OpenSearchPalette, cmd(Key::F), "Search photos"),
    (AppAction::ZoomIn, plain(Key::Plus), "Zoom in"),
    (AppAction::ZoomIn, plain(Key::Equals), "Zoom in"),
    (AppAction::ZoomOut, plain(Key::Minus), "Zoom out"),
    (
        AppAction::ToggleDiagnostics,
        plain(Key::F12),
        "Toggle diagnostics",
    ),
    (AppAction::Quit, cmd(Key::Q), "Quit"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_fold_alias_chords_and_list_the_fixed_keys() {
        let rows = shortcuts();
        assert!(!rows.is_empty());

        // ZoomIn binds both `+` and `=`, but the reference shows it once.
        let zoom_in = rows.iter().filter(|s| s.description == "Zoom in").count();
        assert_eq!(zoom_in, 1, "alias chords must fold to one row");

        // The Space → ToggleGallery binding flows through KEYMAP, not the fixed
        // tail, so it must appear.
        assert!(rows.iter().any(|s| s.description == "Open / close gallery"));

        // The fixed non-registry keys are appended.
        for desc in ["Move focus", "Clear selection / close"] {
            assert!(
                rows.iter().any(|s| s.description == desc),
                "missing fixed shortcut: {desc}"
            );
        }
    }
}
