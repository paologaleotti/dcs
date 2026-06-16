//! The command registry (§2.10): one catalog of invocable actions shared by
//! every surface — keyboard, the `Cmd/Ctrl+P` palette, and menus. The UI never
//! decides what an action *does*; it asks [`catalog`] for the available actions,
//! calls [`Session::run_action`] with one, and performs whatever shell-level
//! [`ActionEffect`] comes back. Everything that can be done in pure app state
//! happens here; only the irreducibly UI/OS bits (file dialog, window close,
//! viewport zoom, modals) escape as effects.
//!
//! Adding a command is three edits, all in this file: a variant on [`AppAction`]
//! plus its `id`, a `run_action` arm, and a line in [`catalog`]. Keys and the
//! palette pick it up automatically.

use std::path::PathBuf;

use crate::session::{Session, VerdictFilter};

/// Every invocable command. `Copy` so it can be matched, listed, and bound to
/// keys freely. Display text and availability live in [`catalog`], not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    OpenFolder,
    OpenRecent(usize),
    Rescan,
    ForgetMissing,
    ClearRecents,
    SetFilter(VerdictFilter),
    ZoomIn,
    ZoomOut,
    ToggleDiagnostics,
    Accept,
    Reject,
    ClearSelection,
    Undo,
    Redo,
    SetShootZone,
    About,
    Quit,
}

impl AppAction {
    /// A stable identifier — the MRU key and the seam a future remappable keymap
    /// or persisted MRU keys on. Never reuse a string across actions.
    pub fn id(self) -> &'static str {
        match self {
            AppAction::OpenFolder => "open-folder",
            AppAction::OpenRecent(_) => "open-recent",
            AppAction::Rescan => "rescan",
            AppAction::ForgetMissing => "forget-missing",
            AppAction::ClearRecents => "clear-recents",
            AppAction::SetFilter(VerdictFilter::All) => "view-all",
            AppAction::SetFilter(VerdictFilter::Unreviewed) => "view-unreviewed",
            AppAction::SetFilter(VerdictFilter::Accepted) => "view-accepted",
            AppAction::SetFilter(VerdictFilter::Rejected) => "view-rejected",
            AppAction::ZoomIn => "zoom-in",
            AppAction::ZoomOut => "zoom-out",
            AppAction::ToggleDiagnostics => "toggle-diagnostics",
            AppAction::Accept => "accept",
            AppAction::Reject => "reject",
            AppAction::ClearSelection => "clear-selection",
            AppAction::Undo => "undo",
            AppAction::Redo => "redo",
            AppAction::SetShootZone => "set-shoot-zone",
            AppAction::About => "about",
            AppAction::Quit => "quit",
        }
    }
}

/// The shell-level work an action needs the UI/OS to do — the only things the
/// app layer can't perform itself (no egui, no rfd, no window handle here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionEffect {
    /// Fully handled in app state; the UI does nothing.
    None,
    /// Open the native folder picker, then open the chosen folder.
    PickFolder,
    /// Open this specific folder (e.g. a recent project).
    OpenPath(PathBuf),
    /// Drop the GPU texture cache (the pool changed underneath it).
    ClearTextures,
    ZoomIn,
    ZoomOut,
    ToggleDiagnostics,
    OpenZonePicker,
    ShowAbout,
    Quit,
}

/// A catalog row: the action plus its display text and grouping. Built per
/// frame from current session state, so only valid actions ever appear.
pub struct ActionEntry {
    pub action: AppAction,
    pub title: String,
    pub category: Category,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    File,
    View,
    Edit,
    Zone,
    App,
}

/// The actions available right now, ordered most-recently-used first (then by
/// catalog order). State-aware: an action only appears when it would do
/// something — no `Undo` with an empty stack, no `Rescan` without a folder.
pub fn catalog(session: &Session) -> Vec<ActionEntry> {
    let mut e: Vec<ActionEntry> = Vec::new();
    let push = |e: &mut Vec<ActionEntry>, action, title: &str, category| {
        e.push(ActionEntry {
            action,
            title: title.to_string(),
            category,
        });
    };

    push(
        &mut e,
        AppAction::OpenFolder,
        "Open Folder…",
        Category::File,
    );
    for (i, path) in session.recent_projects().iter().enumerate() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        e.push(ActionEntry {
            action: AppAction::OpenRecent(i),
            title: format!("Open Recent: {name}"),
            category: Category::File,
        });
    }
    if session.has_folder() {
        push(&mut e, AppAction::Rescan, "Rescan Folder", Category::File);
    }
    let missing = session.missing_count();
    if missing > 0 {
        e.push(ActionEntry {
            action: AppAction::ForgetMissing,
            title: format!("Remove Missing ({missing})"),
            category: Category::File,
        });
    }
    if !session.recent_projects().is_empty() {
        push(
            &mut e,
            AppAction::ClearRecents,
            "Clear Recents",
            Category::File,
        );
    }

    let f = session.filter();
    push_filter(&mut e, "View: All", VerdictFilter::All, f);
    push_filter(&mut e, "View: Unreviewed", VerdictFilter::Unreviewed, f);
    push_filter(&mut e, "View: Accepted", VerdictFilter::Accepted, f);
    push_filter(&mut e, "View: Rejected", VerdictFilter::Rejected, f);
    push(&mut e, AppAction::ZoomIn, "Zoom In", Category::View);
    push(&mut e, AppAction::ZoomOut, "Zoom Out", Category::View);
    push(
        &mut e,
        AppAction::ToggleDiagnostics,
        "Toggle Diagnostics",
        Category::View,
    );

    if session.selection_count() > 0 {
        push(
            &mut e,
            AppAction::Accept,
            "Accept Selection",
            Category::Edit,
        );
        push(
            &mut e,
            AppAction::Reject,
            "Reject Selection",
            Category::Edit,
        );
        push(
            &mut e,
            AppAction::ClearSelection,
            "Clear Selection",
            Category::Edit,
        );
    }
    if session.can_undo() {
        push(&mut e, AppAction::Undo, "Undo", Category::Edit);
    }
    if session.can_redo() {
        push(&mut e, AppAction::Redo, "Redo", Category::Edit);
    }

    push(
        &mut e,
        AppAction::SetShootZone,
        "Set Shoot Timezone…",
        Category::Zone,
    );

    push(&mut e, AppAction::About, "About dcs", Category::App);
    push(&mut e, AppAction::Quit, "Quit", Category::App);

    order_by_mru(&mut e, session.action_mru());
    e
}

impl Session {
    /// Run a command: do everything possible in app state (recording it in the
    /// MRU) and return the shell work the UI must finish. The single dispatch
    /// point behind every surface.
    pub fn run_action(&mut self, action: AppAction) -> ActionEffect {
        self.note_action(action.id());
        match action {
            AppAction::OpenFolder => ActionEffect::PickFolder,
            AppAction::OpenRecent(i) => match self.recent_projects().get(i) {
                Some(path) => ActionEffect::OpenPath(path.clone()),
                None => ActionEffect::None,
            },
            AppAction::Rescan => {
                self.rescan();
                ActionEffect::ClearTextures
            }
            AppAction::ForgetMissing => {
                self.forget_missing();
                ActionEffect::ClearTextures
            }
            AppAction::ClearRecents => {
                self.clear_recents();
                ActionEffect::None
            }
            AppAction::SetFilter(filter) => {
                self.set_filter(filter);
                ActionEffect::None
            }
            AppAction::ZoomIn => ActionEffect::ZoomIn,
            AppAction::ZoomOut => ActionEffect::ZoomOut,
            AppAction::ToggleDiagnostics => ActionEffect::ToggleDiagnostics,
            AppAction::Accept => {
                self.accept();
                ActionEffect::None
            }
            AppAction::Reject => {
                self.reject();
                ActionEffect::None
            }
            AppAction::ClearSelection => {
                self.clear_selection();
                ActionEffect::None
            }
            AppAction::Undo => {
                self.undo();
                ActionEffect::None
            }
            AppAction::Redo => {
                self.redo();
                ActionEffect::None
            }
            AppAction::SetShootZone => ActionEffect::OpenZonePicker,
            AppAction::About => ActionEffect::ShowAbout,
            AppAction::Quit => ActionEffect::Quit,
        }
    }
}

fn push_filter(
    e: &mut Vec<ActionEntry>,
    title: &str,
    filter: VerdictFilter,
    active: VerdictFilter,
) {
    // The active view is a no-op — leave it out so the palette only offers a
    // change.
    if filter == active {
        return;
    }
    e.push(ActionEntry {
        action: AppAction::SetFilter(filter),
        title: title.to_string(),
        category: Category::View,
    });
}

/// Stable-sort entries so recently-used actions float to the top; everything
/// not in the MRU keeps its catalog order.
fn order_by_mru(entries: &mut [ActionEntry], mru: &[&'static str]) {
    let rank = |id: &'static str| mru.iter().position(|&m| m == id).unwrap_or(usize::MAX);
    entries.sort_by_key(|entry| rank(entry.action.id()));
}
