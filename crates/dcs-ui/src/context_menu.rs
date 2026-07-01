//! The right-click menu vocabulary, shared by the grid (cell + header menus)
//! and the gallery (photo + filmstrip). Every row resolves to an [`AppAction`]
//! dispatched through the one registry path, so a menu can never do anything the
//! keys and palette can't. The surfaces that own the layout (grid, gallery)
//! resolve *what* was clicked into a [`MenuTarget`] and call [`show_menu`]; this
//! module owns only the menu contents.

use std::collections::HashSet;

use dcs_app::{AppAction, Session};
use egui::{RichText, Ui};

/// Minimum width for a right-click menu, so its rows never wrap — a menu opened
/// inside a narrow container (the board's sidebar panel) would otherwise inherit
/// that width and wrap each label character by character.
const MENU_MIN_WIDTH: f32 = 200.0;

/// What a right-click landed on, resolved by the clicked surface and remembered
/// across frames (a context menu re-runs its body every frame while open). The
/// group's `count`/`has_tags` are captured at click time so the open menu never
/// re-scans the pool per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuTarget {
    /// A photo cell, by display index.
    Cell(usize),
    /// A group header, by index into `Session::groups`, plus the facts the menu
    /// shows — settled when the header was clicked.
    Group {
        idx: usize,
        count: usize,
        has_tags: bool,
    },
}

/// Render the menu for `target` and return any chosen action. `collapsed` +
/// `toggle` carry the ephemeral per-group collapse state, which is view state,
/// not a registry command — a collapse pick sets `toggle` instead of returning
/// an action. A `None` target closes the menu (nothing under the click).
pub(crate) fn show_menu(
    ui: &mut Ui,
    session: &Session,
    target: Option<MenuTarget>,
    collapsed: &HashSet<String>,
    toggle: &mut Option<String>,
) -> Option<AppAction> {
    match target {
        Some(MenuTarget::Cell(idx)) => cell_menu(ui, session, idx),
        Some(MenuTarget::Group {
            idx,
            count,
            has_tags,
        }) => group_menu(ui, session, idx, count, has_tags, collapsed, toggle),
        None => {
            ui.close();
            None
        }
    }
}

/// A pick from the board canvas image menu: a shared registry action (targeting
/// the pool selection the caller aimed at the clicked photo) or a board-native
/// arrangement op the board applies directly.
pub(crate) enum BoardItemPick {
    Registry(AppAction),
    Raise,
    Remove,
}

/// The right-click menu for a placed board image: board-native arrangement rows
/// (bring-to-front, remove-from-board) plus the shared photo menu when the photo
/// is in the visible order (so the registry can target it). `idx` is the photo's
/// display index if shown; when absent, only the arrangement rows appear.
pub(crate) fn board_item_menu(
    ui: &mut Ui,
    session: &Session,
    idx: Option<usize>,
) -> Option<BoardItemPick> {
    ui.set_min_width(MENU_MIN_WIDTH);
    let mut pick = None;
    if ui.button("Bring to front").clicked() {
        pick = Some(BoardItemPick::Raise);
        ui.close();
    }
    if ui.button("Remove from board").clicked() {
        pick = Some(BoardItemPick::Remove);
        ui.close();
    }
    if let Some(idx) = idx {
        ui.separator();
        if let Some(action) = cell_menu(ui, session, idx) {
            pick = Some(BoardItemPick::Registry(action));
        }
    }
    pick
}

/// The photo/selection menu (right-click a cell, or the gallery's photo /
/// filmstrip thumb). Acts on the current selection-or-focus, which the caller
/// has already aimed at the clicked photo.
pub(crate) fn cell_menu(ui: &mut Ui, session: &Session, idx: usize) -> Option<AppAction> {
    ui.set_min_width(MENU_MIN_WIDTH);
    let mut action = None;
    let mut pick = |ui: &mut Ui, label: &str, a: AppAction| {
        if ui.button(label).clicked() {
            action = Some(a);
            ui.close();
        }
    };
    pick(ui, "Accept", AppAction::Accept);
    pick(ui, "Reject", AppAction::Reject);
    // Crop the clicked photo (the caller already aimed focus/selection at it).
    // JPEG only — RAW-only photos aren't croppable in v1.
    if session.is_croppable(idx) {
        pick(ui, "Crop & straighten…", AppAction::EnterCrop);
    }
    ui.separator();
    pick(ui, "Add tag…", AppAction::OpenTagPalette);
    if session.selection_has_tags() {
        pick(ui, "Remove tag…", AppAction::OpenUntagPalette);
    }
    if let Some(group) = session.group_of_index(idx) {
        ui.separator();
        let title = session.group_title(group).unwrap_or("group");
        pick(
            ui,
            &format!("Select all in {title}"),
            AppAction::SelectGroup(group),
        );
    }
    ui.separator();
    pick(ui, "Photo info", AppAction::ShowMetadata);
    pick(ui, "Reveal in file manager", AppAction::RevealSelection);
    action
}

/// The group/band menu (right-click a header). Batch ops over the group's
/// members plus the ephemeral collapse toggle. `count`/`has_tags` come from the
/// click, so this never touches the pool while the menu is open.
fn group_menu(
    ui: &mut Ui,
    session: &Session,
    idx: usize,
    count: usize,
    has_tags: bool,
    collapsed: &HashSet<String>,
    toggle: &mut Option<String>,
) -> Option<AppAction> {
    ui.set_min_width(MENU_MIN_WIDTH);
    let mut action = None;
    let Some(title) = session.group_title(idx) else {
        ui.close();
        return None;
    };
    ui.label(
        RichText::new(format!("{title} · {count} photos"))
            .monospace()
            .weak(),
    );
    ui.separator();
    let mut pick = |ui: &mut Ui, label: &str, a: AppAction| {
        if ui.button(label).clicked() {
            action = Some(a);
            ui.close();
        }
    };
    pick(ui, "Select all", AppAction::SelectGroup(idx));
    ui.separator();
    pick(ui, "Accept all", AppAction::AcceptGroup(idx));
    pick(ui, "Reject all", AppAction::RejectGroup(idx));
    ui.separator();
    pick(ui, "Add tag to all…", AppAction::TagGroup(idx));
    if has_tags {
        pick(ui, "Remove tag from all…", AppAction::UntagGroup(idx));
    }
    ui.separator();
    let label = if collapsed.contains(title) {
        "Expand"
    } else {
        "Collapse"
    };
    if ui.button(label).clicked() {
        *toggle = Some(title.to_string());
        ui.close();
    }
    action
}
