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

use dcs_domain::grouping::{Axis, TimeGranularity};
use dcs_domain::sort::{Sort, SortDir, SortKey};

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
    GroupBy(Axis),
    SetGranularity(TimeGranularity),
    CollapseAllGroups,
    ExpandAllGroups,
    SetSort(Sort),
    ZoomIn,
    ZoomOut,
    ToggleDiagnostics,
    Accept,
    Reject,
    ClearSelection,
    Undo,
    Redo,
    SetShootZone,
    SetCameraZone,
    ShowMetadata,
    OpenExport,
    RevealRejected,
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
            AppAction::GroupBy(Axis::Time(_)) => "group-time",
            AppAction::GroupBy(Axis::None) => "group-none",
            AppAction::SetGranularity(TimeGranularity::Auto) => "gran-auto",
            AppAction::SetGranularity(TimeGranularity::SmartDay) => "gran-smart-day",
            AppAction::SetGranularity(TimeGranularity::Hour) => "gran-hour",
            AppAction::SetGranularity(TimeGranularity::Day) => "gran-day",
            AppAction::SetGranularity(TimeGranularity::Week) => "gran-week",
            AppAction::CollapseAllGroups => "collapse-all-groups",
            AppAction::ExpandAllGroups => "expand-all-groups",
            AppAction::SetSort(Sort {
                key: SortKey::Time,
                dir: SortDir::Asc,
            }) => "sort-time-asc",
            AppAction::SetSort(Sort {
                key: SortKey::Time,
                dir: SortDir::Desc,
            }) => "sort-time-desc",
            AppAction::SetSort(Sort {
                key: SortKey::Name,
                dir: SortDir::Asc,
            }) => "sort-name-asc",
            AppAction::SetSort(Sort {
                key: SortKey::Name,
                dir: SortDir::Desc,
            }) => "sort-name-desc",
            AppAction::ZoomIn => "zoom-in",
            AppAction::ZoomOut => "zoom-out",
            AppAction::ToggleDiagnostics => "toggle-diagnostics",
            AppAction::Accept => "accept",
            AppAction::Reject => "reject",
            AppAction::ClearSelection => "clear-selection",
            AppAction::Undo => "undo",
            AppAction::Redo => "redo",
            AppAction::SetShootZone => "set-shoot-zone",
            AppAction::SetCameraZone => "set-camera-zone",
            AppAction::ShowMetadata => "show-metadata",
            AppAction::OpenExport => "open-export",
            AppAction::RevealRejected => "reveal-rejected",
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
    OpenCameraZonePicker,
    ShowMetadata,
    ShowAbout,
    CollapseAllGroups,
    ExpandAllGroups,
    /// Open the export dialog (it owns the staged settings + live preview).
    OpenExport,
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
    Group,
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

    push_group_actions(&mut e, session);

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
    if session.focus().is_some() {
        push(
            &mut e,
            AppAction::ShowMetadata,
            "Photo Info / Metadata",
            Category::View,
        );
    }
    if session.can_undo() {
        push(&mut e, AppAction::Undo, "Undo", Category::Edit);
    }
    if session.can_redo() {
        push(&mut e, AppAction::Redo, "Redo", Category::Edit);
    }

    if session.pool_len() > 0 {
        push(&mut e, AppAction::OpenExport, "Export…", Category::File);
    }
    if session.has_rejected() {
        push(
            &mut e,
            AppAction::RevealRejected,
            "Reveal Rejected in File Manager",
            Category::File,
        );
    }

    push(
        &mut e,
        AppAction::SetShootZone,
        "Set Travel Timezone…",
        Category::Zone,
    );
    push(
        &mut e,
        AppAction::SetCameraZone,
        "Set Camera Timezone…",
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
            AppAction::GroupBy(axis) => {
                self.set_axis(axis);
                ActionEffect::None
            }
            AppAction::SetGranularity(g) => {
                self.set_axis(Axis::Time(g));
                ActionEffect::None
            }
            AppAction::CollapseAllGroups => ActionEffect::CollapseAllGroups,
            AppAction::ExpandAllGroups => ActionEffect::ExpandAllGroups,
            AppAction::SetSort(sort) => {
                self.set_sort(sort);
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
            AppAction::SetCameraZone => ActionEffect::OpenCameraZonePicker,
            AppAction::ShowMetadata => ActionEffect::ShowMetadata,
            AppAction::OpenExport => ActionEffect::OpenExport,
            AppAction::RevealRejected => {
                self.reveal_rejected();
                ActionEffect::None
            }
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
    // The active view is omitted — the palette only offers a change.
    if filter == active {
        return;
    }
    e.push(ActionEntry {
        action: AppAction::SetFilter(filter),
        title: title.to_string(),
        category: Category::View,
    });
}

/// Grouping + sort entries (§2.3, §2.8). The axis switch is always offered; the
/// granularity sub-options appear only while grouping by time. The active
/// choice is omitted so the palette only ever offers a change.
fn push_group_actions(e: &mut Vec<ActionEntry>, session: &Session) {
    let axis = session.axis();
    let group = |e: &mut Vec<ActionEntry>, action, title: &str| {
        e.push(ActionEntry {
            action,
            title: title.to_string(),
            category: Category::Group,
        });
    };

    if axis != Axis::None {
        group(e, AppAction::GroupBy(Axis::None), "Group by: None");
    }
    for (g, title) in [
        (TimeGranularity::Auto, "Group by: Auto"),
        (TimeGranularity::SmartDay, "Group by: Smart day"),
        (TimeGranularity::Hour, "Group by: Hour"),
        (TimeGranularity::Day, "Group by: Day"),
        (TimeGranularity::Week, "Group by: Week"),
    ] {
        if axis != Axis::Time(g) {
            group(e, AppAction::SetGranularity(g), title);
        }
    }
    // Collapse/expand only make sense when groups have headers (not the stream).
    if axis != Axis::None {
        group(e, AppAction::CollapseAllGroups, "Group: Collapse All");
        group(e, AppAction::ExpandAllGroups, "Group: Expand All");
    }

    let active = session.sort();
    for (key, name) in [(SortKey::Time, "Time"), (SortKey::Name, "Name")] {
        for (dir, word) in [(SortDir::Asc, "↑ asc"), (SortDir::Desc, "↓ desc")] {
            let sort = Sort { key, dir };
            if sort != active {
                group(e, AppAction::SetSort(sort), &format!("Sort: {name} {word}"));
            }
        }
    }
}

/// Stable-sort entries so recently-used actions float to the top; everything
/// not in the MRU keeps its catalog order.
fn order_by_mru(entries: &mut [ActionEntry], mru: &[&'static str]) {
    let rank = |id: &'static str| mru.iter().position(|&m| m == id).unwrap_or(usize::MAX);
    entries.sort_by_key(|entry| rank(entry.action.id()));
}
